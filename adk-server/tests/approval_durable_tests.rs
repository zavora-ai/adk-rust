//! The approval service and the durable worker together: a run that holds a call parks,
//! its approval request carries the run id, and the decision that meets the quorum wakes
//! exactly that run, which resumes and executes the call once.

#![cfg(feature = "background")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::principal::SubjectRef;
use adk_core::run_store::RunCheckpointRef;
use adk_core::{
    ApprovalDecision, ApprovalPolicy, ApprovalService, ApprovalState, ApproverSelector, Content,
    DurableRunStore, InMemoryActionLedger, InMemoryApprovalStore, InMemoryRunStore, InboxQuery,
    Llm, LlmRequest, LlmResponse, LlmResponseStream, NewRun, Part, Result, RunConfig, RunRecord,
    RunState, StoreApprovalService, Tool, ToolContext, ToolEffect,
};
use adk_runner::Runner;
use adk_server::background::{DurableWorker, DurableWorkerConfig};
use adk_session::{InMemorySessionService, SessionService};
use async_trait::async_trait;
use serde_json::{Value, json};

/// Issues `transfer` until it has a real (not pending) response, then reports it.
struct TransferModel;

#[async_trait]
impl Llm for TransferModel {
    fn name(&self) -> &str {
        "transfer-script"
    }

    async fn generate_content(&self, req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let answered = req.contents.iter().flat_map(|c| &c.parts).find_map(|part| match part {
            Part::FunctionResponse { function_response, .. }
                if function_response.name == "transfer"
                    && !adk_core::is_confirmation_pending(&function_response.response) =>
            {
                Some(function_response.response.clone())
            }
            _ => None,
        });
        let content = match answered {
            Some(response) => Content::new("model").with_text(format!("transfer: {response}")),
            None => Content {
                role: "model".to_string(),
                parts: vec![Part::FunctionCall {
                    name: "transfer".to_string(),
                    args: json!({ "amountCents": 5000 }),
                    id: Some("call-transfer-1".to_string()),
                    thought_signature: None,
                }],
            },
        };
        let response =
            LlmResponse { content: Some(content), turn_complete: true, ..Default::default() };
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

struct TransferTool {
    executions: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for TransferTool {
    fn name(&self) -> &str {
        "transfer"
    }

    fn description(&self) -> &str {
        "moves money"
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "status": "sent" }))
    }
}

async fn wait_for_run(
    runs: &Arc<dyn DurableRunStore>,
    run_id: &str,
    done: impl Fn(&RunRecord) -> bool,
) -> RunRecord {
    let started = tokio::time::Instant::now();
    loop {
        let run = runs.get(run_id).await.unwrap().unwrap();
        if done(&run) {
            return run;
        }
        assert!(started.elapsed() < Duration::from_secs(10), "run never got there: {run:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn finance(name: &str) -> ApprovalDecision {
    ApprovalDecision::approve(SubjectRef::human(name)).with_groups(["finance"])
}

#[tokio::test]
async fn a_quorum_decision_wakes_the_parked_run_and_the_call_executes_once() {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let approvals = Arc::new(InMemoryApprovalStore::new());
    let service = Arc::new(
        StoreApprovalService::new(approvals.clone())
            .with_run_store(Arc::clone(&runs))
            .with_router(ApprovalPolicy::new([ApproverSelector::group("finance")]).with_quorum(2)),
    );
    let executions = Arc::new(AtomicUsize::new(0));
    let agent = LlmAgentBuilder::new("ops")
        .model(Arc::new(TransferModel) as Arc<dyn Llm>)
        .tool(Arc::new(TransferTool { executions: Arc::clone(&executions) }))
        .require_tool_confirmation("transfer")
        .build()
        .unwrap();
    let runner = Arc::new(
        Runner::builder()
            .app_name("ops")
            .agent(Arc::new(agent))
            .session_service(Arc::new(InMemorySessionService::new()) as Arc<dyn SessionService>)
            .run_config(RunConfig::builder().approval_service(service.clone()).build())
            .action_ledger(Arc::new(InMemoryActionLedger::new()))
            .build()
            .unwrap(),
    );
    let worker = DurableWorker::new(Arc::clone(&runs))
        .with_agent("ops", runner)
        .with_config(
            DurableWorkerConfig::default()
                .with_worker_id("w")
                .with_lease_ttl(Duration::from_millis(300))
                .with_poll_interval(Duration::from_millis(20)),
        )
        .spawn();
    let run_id = runs
        .enqueue(
            NewRun::new("default", "ops", "alice", "ops", json!("send 50 USD"))
                .with_session_id("pay-1"),
        )
        .await
        .unwrap()
        .record()
        .unwrap()
        .run_id
        .clone();

    // 1. The run holds the call and parks; its request carries the run id.
    let parked = wait_for_run(&runs, &run_id, |run| run.state == RunState::Waiting).await;
    assert!(matches!(parked.checkpoint, Some(RunCheckpointRef::AwaitingApproval { .. })));
    let inbox = service.inbox(InboxQuery::for_group("finance")).await.unwrap();
    let [request] = inbox.items.as_slice() else {
        panic!("expected one approval request: {inbox:?}");
    };
    assert_eq!(request.run_id.as_deref(), Some(run_id.as_str()));
    assert_eq!(executions.load(Ordering::SeqCst), 0);

    // 2. One of two approvals: the run stays parked.
    let half = service.decide(&request.id, request.version, finance("ana")).await.unwrap();
    assert_eq!(half.state, ApprovalState::Pending);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(runs.get(&run_id).await.unwrap().unwrap().state, RunState::Waiting);
    assert_eq!(executions.load(Ordering::SeqCst), 0);

    // 3. The quorum is met: the decision wakes the run, which resumes and executes once.
    let resolved = service.decide(&half.id, half.version, finance("bo")).await.unwrap();
    assert_eq!(resolved.state, ApprovalState::Approved);
    let finished = wait_for_run(&runs, &run_id, |run| run.state.is_terminal()).await;
    worker.shutdown().await;
    assert_eq!(finished.state, RunState::Completed, "{finished:?}");
    assert_eq!(finished.attempt, 1, "the wake resumes the same attempt");
    assert_eq!(executions.load(Ordering::SeqCst), 1, "the approved call executes once");
    assert_eq!(
        finished.result.as_ref().unwrap()["text"],
        format!("transfer: {}", json!({ "status": "sent" }))
    );
}
