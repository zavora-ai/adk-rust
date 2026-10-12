//! The runtime audit trail: what a governed run records, in what order, and that an
//! action whose record cannot be written does not happen.
//!
//! Every test drives a real `LlmAgent` through a `Runner` with a scripted model.

use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_auth::signed_audit::{AuditExport, AuditLog, AuditSigner, Ed25519Signer, NoteVerifier};
use adk_core::ActingConfig;
use adk_core::audit_log::{
    AuditActor, AuditDecision, AuditFailureMode, AuditRecordDraft, AuditRecordKind,
    AuditRecordOutcome, AuditRecorder, InMemoryAuditLogStore, InMemoryAuditRecorder,
};
use adk_core::principal::{AgentId, AgentPrincipal, SubjectKind, SubjectRef};
use adk_core::{
    AdkError, Content, DeclarativePolicy, Event, InMemoryActionLedger, Llm, LlmRequest,
    LlmResponse, LlmResponseStream, Part, Result, RunConfig, SessionId, Tool, ToolApproval,
    ToolConfirmationRequest, ToolContext, ToolEffect, UserId,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// --- Harness ---

struct ScriptedModel {
    responses: Mutex<VecDeque<LlmResponse>>,
}

impl ScriptedModel {
    fn new(responses: Vec<LlmResponse>) -> Arc<Self> {
        Arc::new(Self { responses: Mutex::new(responses.into()) })
    }
}

#[async_trait]
impl Llm for ScriptedModel {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn generate_content(&self, _req: LlmRequest, _stream: bool) -> Result<LlmResponseStream> {
        let response = self.responses.lock().unwrap().pop_front().unwrap_or_else(|| LlmResponse {
            content: Some(Content::new("model").with_text("done")),
            turn_complete: true,
            ..Default::default()
        });
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}

fn calls(calls: &[(&str, Value, &str)]) -> LlmResponse {
    LlmResponse {
        content: Some(Content {
            role: "model".to_string(),
            parts: calls
                .iter()
                .map(|(name, args, id)| Part::FunctionCall {
                    name: (*name).to_string(),
                    args: args.clone(),
                    id: Some((*id).to_string()),
                    thought_signature: None,
                })
                .collect(),
        }),
        turn_complete: true,
        ..Default::default()
    }
}

struct RecordingTool {
    name: &'static str,
    effect: ToolEffect,
    hang: Option<Duration>,
    executions: Arc<Mutex<Vec<Value>>>,
}

impl RecordingTool {
    fn new(name: &'static str) -> (Arc<Self>, Arc<Mutex<Vec<Value>>>) {
        let executions = Arc::new(Mutex::new(Vec::new()));
        let tool = Self {
            name,
            effect: ToolEffect::NonIdempotent,
            hang: None,
            executions: executions.clone(),
        };
        (Arc::new(tool), executions)
    }
}

#[async_trait]
impl Tool for RecordingTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "records its arguments"
    }

    fn effect(&self) -> ToolEffect {
        self.effect
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        self.executions.lock().unwrap().push(args);
        if let Some(hang) = self.hang {
            tokio::time::sleep(hang).await;
        }
        Ok(json!({ "status": "ok" }))
    }
}

type RunnerBuilder = adk_runner::RunnerConfigBuilder<
    adk_runner::builder::HasAppName,
    adk_runner::builder::HasAgent,
    adk_runner::builder::HasSessionService,
>;

async fn runner(agent: LlmAgent, configure: impl FnOnce(RunnerBuilder) -> RunnerBuilder) -> Runner {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "bank".to_string(),
            user_id: "alice".to_string(),
            session_id: Some("session".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let builder = Runner::builder()
        .app_name("bank")
        .agent(Arc::new(agent))
        .session_service(sessions as Arc<dyn SessionService>);
    configure(builder).build().unwrap()
}

async fn run(runner: &Runner, config: Option<RunConfig>) -> Vec<Result<Event>> {
    let stream = runner
        .run_with_config(
            UserId::new("alice").unwrap(),
            SessionId::new("session").unwrap(),
            Content::new("user").with_text("go"),
            config,
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), stream.collect::<Vec<_>>())
        .await
        .expect("the run must finish")
}

/// A policy allowing `search`, denying `delete_file`, and holding `transfer` for approval.
fn bank_policy() -> Arc<DeclarativePolicy> {
    Arc::new(
        DeclarativePolicy::builder()
            .name("bank-v1")
            .allow("search")
            .deny("delete_file", "deleting files is not permitted")
            .require_approval("transfer", "transfers need a person")
            .build(),
    )
}

fn bank_agent(model: Arc<ScriptedModel>, tools: Vec<Arc<RecordingTool>>) -> LlmAgent {
    let mut builder = LlmAgentBuilder::new("teller")
        .model(model)
        .tool_execution_strategy(adk_core::ToolExecutionStrategy::Sequential);
    for tool in tools {
        builder = builder.tool(tool);
    }
    builder.build().unwrap()
}

/// The actor the runtime derives for the `teller` agent acting for `alice`.
fn teller(invocation_id: Option<String>) -> AuditActor {
    AuditActor {
        agent: Some(AgentId::new("urn:adk:agent:bank/teller").unwrap()),
        on_behalf_of: Some(SubjectRef::human("alice")),
        invocation_id,
        ..AuditActor::default()
    }
}

/// The drafts the governed path writes for one call, given the record's invocation.
fn tool_draft(
    recorder: &InMemoryAuditRecorder,
    invocation_id: Option<String>,
    tool: &str,
    call_id: &str,
    args: &Value,
) -> AuditRecordDraft {
    AuditRecordDraft::new(AuditRecordKind::ToolCall, tool)
        .actor(teller(invocation_id))
        .args_digest(recorder.digest(args))
        .ext("functionCallId", json!(call_id))
        .ext("effect", json!("non_idempotent"))
        .ext("sessionId", json!("session"))
}

/// Fails every write, as an unreachable audit store does.
struct BrokenRecorder;

#[async_trait]
impl AuditRecorder for BrokenRecorder {
    fn digest(&self, value: &Value) -> String {
        adk_core::json_digest(value)
    }

    async fn record(&self, _draft: AuditRecordDraft) -> Result<()> {
        Err(AdkError::session("audit store unreachable"))
    }
}

// --- Ordered records ---

#[tokio::test]
async fn one_run_records_each_decision_in_order_then_the_approval_that_resumes_the_held_call() {
    let search_args = json!({ "q": "balance" });
    let delete_args = json!({ "path": "/ledger" });
    let transfer_args = json!({ "to": "acct-secret-9917", "amount": 250 });
    let model = ScriptedModel::new(vec![calls(&[
        ("search", search_args.clone(), "c1"),
        ("delete_file", delete_args.clone(), "c2"),
        ("transfer", transfer_args.clone(), "c3"),
    ])]);
    let (search, searches) = RecordingTool::new("search");
    let (delete, deletes) = RecordingTool::new("delete_file");
    let (transfer, transfers) = RecordingTool::new("transfer");
    let recorder = InMemoryAuditRecorder::new();
    let agent = bank_agent(model, vec![search, delete, transfer]);
    let audit = recorder.clone();
    let runner =
        runner(agent, move |builder| builder.tool_policy(bank_policy()).audit_log(Arc::new(audit)))
            .await;

    let events = run(&runner, None).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!(searches.lock().unwrap().len(), 1);
    assert!(deletes.lock().unwrap().is_empty());
    assert!(transfers.lock().unwrap().is_empty(), "the transfer is held");

    let first = recorder.records();
    let invocation = first[0].actor.invocation_id.clone();
    assert!(invocation.as_deref().is_some_and(|id| id.starts_with("inv-")), "{invocation:?}");
    assert_eq!(
        first,
        vec![
            tool_draft(&recorder, invocation.clone(), "search", "c1", &search_args)
                .decision(AuditDecision::Allowed)
                .policy_ref("tool_policy:bank-v1"),
            tool_draft(&recorder, invocation.clone(), "delete_file", "c2", &delete_args)
                .decision(AuditDecision::Denied)
                .outcome(AuditRecordOutcome::NotRun)
                .policy_ref("tool_policy:bank-v1")
                .ext("reason", json!("deleting files is not permitted")),
            tool_draft(&recorder, invocation.clone(), "transfer", "c3", &transfer_args)
                .decision(AuditDecision::ApprovalRequired)
                .outcome(AuditRecordOutcome::NotRun)
                .policy_ref("tool_policy:bank-v1")
                .ext("reason", json!("transfers need a person")),
        ]
    );

    // An approver approves the held transfer; the next run resumes it.
    let request = ToolConfirmationRequest {
        tool_name: "transfer".into(),
        function_call_id: Some("c3".into()),
        args: transfer_args.clone(),
        acting: None,
    };
    let config =
        RunConfig::builder().tool_approval(request.fingerprint(), ToolApproval::approve()).build();
    let events = run(&runner, Some(config)).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!(*transfers.lock().unwrap(), vec![transfer_args.clone()]);

    let resumed = recorder.records()[first.len()..].to_vec();
    let second = resumed[0].actor.invocation_id.clone();
    assert_ne!(second, invocation, "the resume is a new invocation");
    let approval = tool_draft(&recorder, second.clone(), "transfer", "c3", &transfer_args);
    let approval = AuditRecordDraft { kind: AuditRecordKind::Approval, ..approval };
    assert_eq!(
        resumed,
        vec![
            approval.decision(AuditDecision::Approved).ext("source", json!("fingerprint")),
            tool_draft(&recorder, second, "transfer", "c3", &transfer_args)
                .decision(AuditDecision::Allowed)
                .policy_ref("confirmation"),
        ]
    );
}

#[tokio::test]
async fn records_name_the_configured_agent_and_the_subject_it_acts_for() {
    const AGENT: &str = "spiffe://bank.example/agent/teller";
    let search_args = json!({ "q": "balance" });
    let delete_args = json!({ "path": "/ledger" });
    let model = ScriptedModel::new(vec![
        calls(&[("search", search_args.clone(), "c1"), ("delete_file", delete_args.clone(), "c2")]),
        LlmResponse {
            content: Some(Content::new("model").with_text("done")),
            turn_complete: true,
            ..Default::default()
        },
        calls(&[("search", search_args.clone(), "c3")]),
    ]);
    let (search, _) = RecordingTool::new("search");
    let (delete, _) = RecordingTool::new("delete_file");
    let recorder = InMemoryAuditRecorder::new();
    let agent = bank_agent(model, vec![search, delete]);
    let audit = recorder.clone();
    let principal = AgentPrincipal::new(AgentId::new(AGENT).unwrap()).with_tenant("acme");
    let runner = runner(agent, move |builder| {
        builder.tool_policy(bank_policy()).audit_log(Arc::new(audit)).agent_principal(principal)
    })
    .await;

    // The runner's principal, acting for the run's user.
    let events = run(&runner, None).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    let records = recorder.records();
    assert_eq!(records.len(), 2, "{records:?}");
    for record in &records {
        assert_eq!(
            record.actor,
            AuditActor {
                agent: Some(AgentId::new(AGENT).unwrap()),
                tenant: Some("acme".to_string()),
                on_behalf_of: Some(SubjectRef::human("alice")),
                invocation_id: records[0].actor.invocation_id.clone(),
                ..AuditActor::default()
            }
        );
    }

    // A run's own acting config names its principal, subject kind, and delegation chain.
    let acting = ActingConfig {
        delegation_chain: vec![SubjectRef::human("ops-lead")],
        ..ActingConfig::default()
    }
    .with_principal("teller", AgentPrincipal::new(AgentId::new(AGENT).unwrap()))
    .with_subject_kind(SubjectKind::Service);
    let config = RunConfig::builder().acting(acting).build();
    let events = run(&runner, Some(config)).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    let delegated = recorder.records()[records.len()..].to_vec();
    assert_eq!(delegated.len(), 1, "{delegated:?}");
    let invocation = delegated[0].actor.invocation_id.clone();
    assert!(invocation.as_deref().is_some_and(|id| id.starts_with("inv-")), "{invocation:?}");
    assert_eq!(
        delegated[0].actor,
        AuditActor {
            agent: Some(AgentId::new(AGENT).unwrap()),
            on_behalf_of: Some(SubjectRef::service("alice")),
            delegation_chain: vec![SubjectRef::human("ops-lead")],
            invocation_id: invocation,
            ..AuditActor::default()
        }
    );
}

#[tokio::test]
async fn records_carry_argument_digests_never_arguments() {
    let secret = "acct-secret-9917";
    let model = ScriptedModel::new(vec![calls(&[
        ("search", json!({ "q": secret }), "c1"),
        ("delete_file", json!({ "path": secret }), "c2"),
        ("transfer", json!({ "to": secret, "amount": 1 }), "c3"),
    ])]);
    let (search, _) = RecordingTool::new("search");
    let (delete, _) = RecordingTool::new("delete_file");
    let (transfer, _) = RecordingTool::new("transfer");
    let store = Arc::new(InMemoryAuditLogStore::new());
    let signer = Arc::new(Ed25519Signer::generate("audit.bank.test").unwrap());
    let log = AuditLog::builder(store.clone(), "replica-1", signer).open().await.unwrap();
    let runner = runner(bank_agent(model, vec![search, delete, transfer]), move |builder| {
        builder.tool_policy(bank_policy()).audit_log(Arc::new(log))
    })
    .await;

    run(&runner, None).await;

    let export = AuditExport::from_store(store.as_ref(), "replica-1").await.unwrap();
    assert_eq!(export.entries.len(), 3);
    for entry in &export.entries {
        let record = entry.record.as_deref().unwrap();
        assert!(!record.contains(secret), "a record leaked an argument: {record}");
        let parsed: Value = serde_json::from_str(record).unwrap();
        let digest = parsed["argsDigest"].as_str().unwrap();
        assert!(digest.starts_with("sha256:") && digest.len() == 71, "{digest}");
    }
}

// --- Signed log, end to end ---

#[tokio::test]
async fn a_run_writes_a_signed_log_that_verifies_and_fails_once_tampered() {
    let model = ScriptedModel::new(vec![calls(&[
        ("search", json!({ "q": "balance" }), "c1"),
        ("delete_file", json!({ "path": "/" }), "c2"),
    ])]);
    let (search, _) = RecordingTool::new("search");
    let (delete, _) = RecordingTool::new("delete_file");
    let store = Arc::new(InMemoryAuditLogStore::new());
    let signer = Arc::new(Ed25519Signer::generate("audit.bank.test").unwrap());
    let trusted: NoteVerifier = signer.verifier_key().parse().unwrap();
    let log = Arc::new(AuditLog::builder(store.clone(), "replica-1", signer).open().await.unwrap());
    let recorder = log.clone();
    let runner = runner(bank_agent(model, vec![search, delete]), move |builder| {
        builder.tool_policy(bank_policy()).audit_log(recorder)
    })
    .await;

    run(&runner, None).await;
    runner
        .governance()
        .freeze_recorded("drill", AuditActor::subject(SubjectRef::human("oncall")))
        .await
        .unwrap();
    runner
        .governance()
        .unfreeze_recorded(AuditActor::subject(SubjectRef::human("oncall")))
        .await
        .unwrap();
    log.checkpoint().await.unwrap();

    let export = AuditExport::from_store(store.as_ref(), "replica-1").await.unwrap();
    let kinds: Vec<(String, String)> = export
        .entries
        .iter()
        .map(|entry| {
            let record: Value = serde_json::from_str(entry.record.as_deref().unwrap()).unwrap();
            (record["kind"].as_str().unwrap().into(), record["action"].as_str().unwrap().into())
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("tool_call".to_string(), "search".to_string()),
            ("tool_call".to_string(), "delete_file".to_string()),
            ("governance".to_string(), "governance.freeze".to_string()),
            ("governance".to_string(), "governance.unfreeze".to_string()),
        ]
    );
    let report = export.verify(std::slice::from_ref(&trusted));
    assert!(report.is_valid(), "{report:?}");

    let mut tampered = export.clone();
    let record = tampered.entries[1].record.take().unwrap();
    tampered.entries[1].record = Some(record.replace("\"denied\"", "\"allowed\""));
    assert!(!tampered.verify(&[trusted]).is_valid(), "an edited record must not verify");
}

// --- Fail closed ---

#[tokio::test]
async fn a_failing_recorder_blocks_the_governed_call() {
    let model = ScriptedModel::new(vec![calls(&[("search", json!({ "q": "x" }), "c1")])]);
    let (search, searches) = RecordingTool::new("search");
    let runner = runner(bank_agent(model, vec![search]), |builder| {
        builder.tool_policy(bank_policy()).audit_log(Arc::new(BrokenRecorder))
    })
    .await;

    let events = run(&runner, None).await;

    assert!(searches.lock().unwrap().is_empty(), "an unrecorded call must not run");
    let error = events.iter().find_map(|event| event.as_ref().err()).expect("the run fails");
    assert_eq!(error.code, "audit.record_failed", "{error}");
}

#[tokio::test]
async fn warn_mode_runs_the_call_without_a_record() {
    let model = ScriptedModel::new(vec![calls(&[("search", json!({ "q": "x" }), "c1")])]);
    let (search, searches) = RecordingTool::new("search");
    let runner = runner(bank_agent(model, vec![search]), |builder| {
        builder
            .tool_policy(bank_policy())
            .audit_log(Arc::new(BrokenRecorder))
            .audit_failure_mode(AuditFailureMode::Warn)
    })
    .await;

    let events = run(&runner, None).await;

    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!(searches.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn an_unrecordable_unfreeze_leaves_execution_frozen() {
    let model = ScriptedModel::new(vec![]);
    let runner =
        runner(bank_agent(model, vec![]), |builder| builder.audit_log(Arc::new(BrokenRecorder)))
            .await;
    let admin = AuditActor::subject(SubjectRef::human("oncall"));

    let frozen = runner.governance().freeze_recorded("incident", admin.clone()).await;
    assert_eq!(frozen.unwrap_err().code, "audit.record_failed");
    assert!(runner.governance().is_frozen(), "a freeze holds even when it cannot be recorded");

    let lifted = runner.governance().unfreeze_recorded(admin).await;
    assert_eq!(lifted.unwrap_err().code, "audit.record_failed");
    assert!(runner.governance().is_frozen(), "an unrecorded unfreeze must not lift the freeze");
}

// --- Outcome unknown ---

#[tokio::test(start_paused = true)]
async fn a_timed_out_non_idempotent_call_records_an_unknown_outcome() {
    let args = json!({ "to": "acct-1", "amount": 5 });
    let model = ScriptedModel::new(vec![calls(&[("transfer", args.clone(), "c1")])]);
    let (transfer, _) = RecordingTool::new("transfer");
    let mut transfer = Arc::into_inner(transfer).unwrap();
    transfer.hang = Some(Duration::from_secs(3600));
    let agent = LlmAgentBuilder::new("teller")
        .model(model)
        .tool(Arc::new(transfer))
        .tool_timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    let recorder = InMemoryAuditRecorder::new();
    let audit = recorder.clone();
    let runner = runner(agent, move |builder| {
        builder.action_ledger(Arc::new(InMemoryActionLedger::new())).audit_log(Arc::new(audit))
    })
    .await;

    run(&runner, None).await;

    let records = recorder.records();
    assert_eq!(records.len(), 2, "{records:?}");
    let invocation = records[0].actor.invocation_id.clone();
    assert_eq!(
        records[0],
        tool_draft(&recorder, invocation.clone(), "transfer", "c1", &args)
            .decision(AuditDecision::Allowed)
    );
    assert_eq!(
        records[1],
        AuditRecordDraft::new(AuditRecordKind::ToolCall, "transfer")
            .actor(teller(invocation))
            .args_digest(recorder.digest(&args))
            .outcome(AuditRecordOutcome::OutcomeUnknown)
            .ext("cause", json!("interrupted"))
            .ext("sessionId", json!("session"))
    );
}
