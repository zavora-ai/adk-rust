use std::time::Duration;

use adk_core::audit_log::{AuditRecorder, InMemoryAuditRecorder};
use adk_core::{InMemoryApprovalStore, InMemoryRunStore, ToolConfirmationRequest};
use chrono::Utc;

use super::*;

fn accept(content: Value) -> Value {
    json!({ "action": "accept", "content": content })
}

#[test]
fn answers_map_to_decisions() {
    assert_eq!(decision_of("k", &json!({ "action": "accept" })).unwrap(), (true, String::new()));
    assert!(decision_of("k", &accept(json!({ "approve": true }))).unwrap().0);
    assert_eq!(
        decision_of("k", &accept(json!({ "approve": false, "reason": "too much" }))).unwrap(),
        (false, "too much".to_string())
    );
    assert_eq!(
        decision_of("k", &json!({ "action": "decline" })).unwrap(),
        (false, "the caller declined the call".to_string())
    );
    assert_eq!(
        decision_of("k", &json!({ "action": "cancel" })).unwrap(),
        (false, "the caller dismissed the request".to_string())
    );
    let error = decision_of("k", &json!({ "approve": true })).unwrap_err();
    assert_eq!(error.code, "mcp.agent.answer");
}

fn pending(at: chrono::DateTime<Utc>) -> PendingApproval {
    PendingApproval {
        fingerprint: "fp-1".to_string(),
        request: ToolConfirmationRequest {
            tool_name: "transfer".to_string(),
            function_call_id: Some("c-1".to_string()),
            args: json!({ "amountCents": 5000 }),
            origin: None,
            acting: None,
        },
        requested_at: at,
    }
}

#[test]
fn a_key_changes_whenever_the_question_does() {
    let at = Utc::now();
    let key = held_key("run-1", &pending(at), None);
    assert!(key.starts_with("approval-"));
    assert_eq!(key, held_key("run-1", &pending(at), None), "the key is stable");
    assert_ne!(key, held_key("run-2", &pending(at), None), "another run");
    let later = at + chrono::Duration::seconds(1);
    assert_ne!(key, held_key("run-1", &pending(later), None), "held again");
    let mut other = pending(at);
    other.fingerprint = "fp-2".to_string();
    assert_ne!(key, held_key("run-1", &other, None), "another call");
}

/// A host over fresh stores, auditing into the recorder, and a run of `ana` parked on the held
/// `transfer`.
async fn parked_run() -> (McpAgentHost, Arc<InMemoryApprovalStore>, RunRecord, InMemoryAuditRecorder)
{
    let runs = Arc::new(InMemoryRunStore::new());
    let approvals = Arc::new(InMemoryApprovalStore::new());
    let recorder = InMemoryAuditRecorder::new();
    let host = McpAgentHost::builder("ops", runs.clone() as Arc<dyn DurableRunStore>)
        .agent(McpAgent::new("ops", "Moves money"))
        .approval_store(approvals.clone())
        .audit(AuditTrail::new(Arc::new(recorder.clone())))
        .poll_interval(Duration::from_millis(10))
        .build()
        .unwrap();
    let run = NewRun::new("default", "ops", "ana", "ops", json!({ "text": "send" }))
        .with_session_id("s-1")
        .with_authenticated_user();
    let run_id = runs.enqueue(run).await.unwrap().record().unwrap().run_id.clone();
    let claimed = runs.claim("default", "w", Duration::from_secs(30)).await.unwrap().unwrap();
    let request = pending(Utc::now()).request;
    approvals.record_pending(&ApprovalScope::new("ops", "ana", "s-1"), &request).await.unwrap();
    let checkpoint = RunCheckpointRef::AwaitingApproval {
        session_id: "s-1".to_string(),
        invocation_id: Some("inv-1".to_string()),
        fingerprints: vec![request.fingerprint()],
        expires_at: None,
    };
    runs.set_checkpoint(&run_id, claimed.epoch, checkpoint).await.unwrap();
    runs.wait(&run_id, claimed.epoch, None).await.unwrap();
    let record = runs.get(&run_id).await.unwrap().unwrap();
    (host, approvals, record, recorder)
}

#[tokio::test]
async fn a_parked_run_is_an_input_required_task_with_one_question_per_held_call() {
    let (host, _, record, _) = parked_run().await;
    let ana = McpCaller::authenticated("ana");
    let task = McpTaskHost::get(&host, &record.run_id, &ana).await.unwrap();
    assert_eq!(task.task.status, TaskStatus::InputRequired);
    assert_eq!(task.task.status_message.as_deref(), Some("waiting for approval to run transfer"));
    assert_eq!(task.task.poll_interval_ms, Some(10));
    let TaskPayload::InputRequired { input_requests } = &task.payload else {
        panic!("{task:?}");
    };
    assert_eq!(input_requests.len(), 1);
    let question = serde_json::to_value(input_requests.values().next().unwrap()).unwrap();
    assert_eq!(question["method"], "elicitation/create");
    assert_eq!(question["params"]["mode"], "form");
    assert_eq!(
        question["params"]["message"],
        "The agent `ops` asks to run `transfer` with {\"amountCents\":5000}. Approve this call?"
    );

    // Only its owner sees it, authenticated as it was.
    for other in [McpCaller::authenticated("bo"), McpCaller::anonymous()] {
        let error = McpTaskHost::get(&host, &record.run_id, &other).await.unwrap_err();
        assert_eq!(error.category, ErrorCategory::NotFound);
    }
}

#[tokio::test]
async fn an_answer_decides_the_held_call_in_the_store_and_wakes_the_run() {
    let (host, approvals, record, recorder) = parked_run().await;
    let ana = McpCaller::authenticated("ana");
    let task = McpTaskHost::get(&host, &record.run_id, &ana).await.unwrap();
    let TaskPayload::InputRequired { input_requests } = task.payload else { panic!() };
    let key = input_requests.keys().next().unwrap().clone();

    // An answer to a question the run does not ask decides nothing.
    let wrong: InputResponses = [("approval-0".to_string(), json!({ "action": "accept" }))].into();
    let stale = McpTaskHost::update(&host, &record.run_id, &ana, wrong).await.unwrap_err();
    assert_eq!((stale.category, stale.code), (ErrorCategory::Conflict, "mcp.agent.stale_answer"));
    let scope = ApprovalScope::new("ops", "ana", "s-1");
    assert_eq!(approvals.pending(&scope).await.unwrap().len(), 1);

    let answers: InputResponses = [(key.clone(), json!({ "action": "accept" }))].into();
    McpTaskHost::update(&host, &record.run_id, &ana, answers.clone()).await.unwrap();
    let fingerprint = pending(Utc::now()).request.fingerprint();
    assert_eq!(
        approvals.take_approval(&scope, &fingerprint).await.unwrap(),
        Some(ToolApproval::approve())
    );
    assert_eq!(
        recorder.records(),
        vec![
            AuditRecordDraft::new(AuditRecordKind::Approval, "transfer")
                .actor(AuditActor::subject(SubjectRef::human("ana")))
                .args_digest(recorder.digest(&json!({ "amountCents": 5000 })))
                .decision(AuditDecision::Approved)
                .ext("source", json!("mcp_elicitation"))
                .ext("runId", json!(record.run_id))
                .ext("sessionId", json!("s-1"))
        ],
        "the decision is audited, once"
    );
    let woken = host.inner.runs.get(&record.run_id).await.unwrap().unwrap();
    let claimed = host.inner.runs.claim("default", "w2", Duration::from_secs(30)).await.unwrap();
    assert_eq!(
        claimed.map(|claim| claim.record.run_id),
        Some(woken.run_id),
        "the wake made it claimable"
    );

    // The same answer again is stale.
    let again = McpTaskHost::update(&host, &record.run_id, &ana, answers).await.unwrap_err();
    assert_eq!(again.code, "mcp.agent.stale_answer");
}

#[tokio::test]
async fn a_finished_run_is_a_completed_task_with_the_answer() {
    let (host, _, record, _) = parked_run().await;
    let runs = &host.inner.runs;
    runs.wake(&record.run_id).await.unwrap();
    let claimed = runs.claim("default", "w2", Duration::from_secs(30)).await.unwrap().unwrap();
    let result = json!({ "text": "sent", "sessionId": "s-1", "invocationId": "inv-1" });
    runs.finish(&record.run_id, claimed.epoch, adk_core::RunOutcome::Completed(result))
        .await
        .unwrap();
    let task =
        McpTaskHost::get(&host, &record.run_id, &McpCaller::authenticated("ana")).await.unwrap();
    let TaskPayload::Completed { result } = &task.payload else { panic!("{task:?}") };
    assert_eq!(
        Value::Object(result.clone()),
        json!({
            "content": [{ "type": "text", "text": "sent" }],
            "structuredContent": { "text": "sent", "sessionId": "s-1", "runId": record.run_id },
            "isError": false,
            "resultType": "complete"
        })
    );
    assert_eq!(task.task.status_message.as_deref(), Some("sent"));
}

#[test]
fn the_builder_needs_an_agent_an_approval_store_and_unique_names() {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let none = McpAgentHost::builder("ops", runs.clone()).build().unwrap_err();
    assert_eq!(none.code, "mcp.agent.none");
    let store = McpAgentHost::builder("ops", runs.clone())
        .agent(McpAgent::new("ops", "Moves money"))
        .build()
        .unwrap_err();
    assert_eq!(store.code, "mcp.agent.approval_store");
    let duplicate = McpAgentHost::builder("ops", runs)
        .agent(McpAgent::new("ops", "Moves money"))
        .agent(McpAgent::new("treasury", "Also").tool_name("ops"))
        .approval_store(Arc::new(InMemoryApprovalStore::new()))
        .build()
        .unwrap_err();
    assert_eq!(duplicate.code, "mcp.agent.duplicate");
}
