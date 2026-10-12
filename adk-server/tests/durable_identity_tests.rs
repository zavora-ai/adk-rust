//! Durable runs and identity: a worker honours a run's stored principal only when it may
//! act as the principal's agent and the principal acts for the run's user, and scheduled
//! runs act for a service.

#![cfg(feature = "background")]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::audit_log::{
    AuditDecision, AuditRecordDraft, AuditRecordKind, AuditRecordOutcome, InMemoryAuditRecorder,
};
use adk_core::schedule::{MissedFirePolicy, Schedule, ScheduleSpec};
use adk_core::{
    ActingContext, AgentId, AgentPrincipal, Content, DurableRunStore, InMemoryRunStore,
    InMemoryScheduleStore, Llm, LlmRequest, LlmResponse, LlmResponseStream, NewRun,
    PRINCIPAL_NOT_ALLOWED_CODE, Part, PolicyDecision, Result, RunRecord, RunState,
    SUBJECT_MISMATCH_CODE, ScheduleStore, StaticSubjectDelegation, SubjectRef, Tool, ToolContext,
    ToolPolicy, ToolPolicyRequest,
};
use adk_runner::Runner;
use adk_server::background::{DurableScheduler, DurableWorker, DurableWorkerConfig};
use adk_session::{InMemorySessionService, SessionService};
use async_trait::async_trait;
use serde_json::{Value, json};

const APP: &str = "ledger";
const AGENT: &str = "clerk";
const TRUSTED: &str = "spiffe://ledger.test/agent/clerk";

/// Calls `lookup` once, then answers.
struct LookupModel {
    responses: Mutex<VecDeque<LlmResponse>>,
}

impl LookupModel {
    fn new() -> Arc<Self> {
        let call = LlmResponse {
            content: Some(Content {
                role: "model".to_string(),
                parts: vec![Part::FunctionCall {
                    name: "lookup".to_string(),
                    args: json!({}),
                    id: Some("c1".to_string()),
                    thought_signature: None,
                }],
            }),
            turn_complete: true,
            ..Default::default()
        };
        Arc::new(Self { responses: Mutex::new(VecDeque::from([call])) })
    }
}

#[async_trait]
impl Llm for LookupModel {
    fn name(&self) -> &str {
        "lookup-script"
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

struct Lookup;

#[async_trait]
impl Tool for Lookup {
    fn name(&self) -> &str {
        "lookup"
    }

    fn description(&self) -> &str {
        "looks up a ledger entry"
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        Ok(json!({ "balance": 10 }))
    }
}

/// Allows every call and records the acting context it decided on.
#[derive(Debug, Default)]
struct RecordingPolicy {
    seen: Mutex<Vec<ActingContext>>,
}

#[async_trait]
impl ToolPolicy for RecordingPolicy {
    async fn evaluate(&self, request: &ToolPolicyRequest) -> PolicyDecision {
        self.seen.lock().unwrap().push(request.acting.clone().unwrap());
        PolicyDecision::Allow
    }
}

struct Fixture {
    runs: Arc<dyn DurableRunStore>,
    policy: Arc<RecordingPolicy>,
    audit: InMemoryAuditRecorder,
    worker: DurableWorker,
}

fn fixture(config: DurableWorkerConfig, delegation: Option<StaticSubjectDelegation>) -> Fixture {
    let runs: Arc<dyn DurableRunStore> = Arc::new(InMemoryRunStore::new());
    let policy = Arc::new(RecordingPolicy::default());
    let audit = InMemoryAuditRecorder::new();
    let agent = LlmAgentBuilder::new(AGENT).model(LookupModel::new()).tool(Arc::new(Lookup));
    let mut builder = Runner::builder()
        .app_name(APP)
        .agent(Arc::new(agent.build().unwrap()))
        .session_service(Arc::new(InMemorySessionService::new()) as Arc<dyn SessionService>)
        .tool_policy(policy.clone())
        .audit_log(Arc::new(audit.clone()));
    if let Some(delegation) = delegation {
        builder = builder.subject_delegation(Arc::new(delegation));
    }
    let runner = Arc::new(builder.build().unwrap());
    let worker = DurableWorker::new(Arc::clone(&runs))
        .with_agent(AGENT, runner)
        .with_config(config.with_worker_id("worker-1").with_lease_ttl(Duration::from_secs(5)));
    Fixture { runs, policy, audit, worker }
}

fn new_run(user: &str) -> NewRun {
    NewRun::new("default", APP, user, AGENT, json!({ "text": "balance?" })).with_max_attempts(1)
}

async fn run_one(fixture: &Fixture, run: NewRun) -> RunRecord {
    fixture.runs.enqueue(run).await.unwrap();
    fixture.worker.run_next().await.unwrap().expect("the run was claimed")
}

fn trusted() -> AgentPrincipal {
    AgentPrincipal::new(AgentId::new(TRUSTED).unwrap()).with_tenant("ledger-co")
}

fn tool_records(audit: &InMemoryAuditRecorder) -> Vec<AuditRecordDraft> {
    audit.records().into_iter().filter(|draft| draft.kind == AuditRecordKind::ToolCall).collect()
}

#[tokio::test]
async fn a_stored_principal_outside_the_allowlist_is_refused_and_audited() {
    let fixture = fixture(DurableWorkerConfig::default(), None);
    let forged = ActingContext::new(trusted()).on_behalf_of(SubjectRef::human("alice"));
    let finished = run_one(&fixture, new_run("alice").with_principal(forged.clone())).await;

    assert_eq!(finished.state, RunState::Failed);
    assert!(
        finished.error.as_deref().is_some_and(|error| error.contains("may not act as")),
        "{finished:?}"
    );
    assert!(fixture.policy.seen.lock().unwrap().is_empty(), "no agent ran");
    let records = fixture.audit.records();
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(
        (&records[0].kind, records[0].action.as_str(), records[0].decision, records[0].outcome),
        (
            &AuditRecordKind::Access,
            PRINCIPAL_NOT_ALLOWED_CODE,
            Some(AuditDecision::Denied),
            Some(AuditRecordOutcome::NotRun)
        )
    );
    assert_eq!(records[0].actor.agent.as_ref().map(AgentId::as_str), Some(TRUSTED));
    assert_eq!(records[0].ext["runId"], json!(finished.run_id));
}

#[tokio::test]
async fn an_allowlisted_principal_is_honoured_for_the_root_agent() {
    let config =
        DurableWorkerConfig::default().with_allowed_principal(AgentId::new(TRUSTED).unwrap());
    let fixture = fixture(config, None);
    let stored = ActingContext::new(trusted()).on_behalf_of(SubjectRef::human("alice"));
    let finished = run_one(&fixture, new_run("alice").with_principal(stored)).await;

    assert_eq!(finished.state, RunState::Completed, "{finished:?}");
    let seen = fixture.policy.seen.lock().unwrap().clone();
    assert_eq!(seen, vec![ActingContext::new(trusted()).on_behalf_of(SubjectRef::human("alice"))]);
    let records = tool_records(&fixture.audit);
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].actor.agent.as_ref().map(AgentId::as_str), Some(TRUSTED));
}

#[tokio::test]
async fn the_derived_principal_needs_no_allowlist() {
    let fixture = fixture(DurableWorkerConfig::default(), None);
    let stored = ActingContext::new(AgentPrincipal::derived(APP, AGENT))
        .on_behalf_of(SubjectRef::human("alice"));
    let finished = run_one(&fixture, new_run("alice").with_principal(stored.clone())).await;

    assert_eq!(finished.state, RunState::Completed, "{finished:?}");
    assert_eq!(*fixture.policy.seen.lock().unwrap(), vec![stored]);
}

#[tokio::test]
async fn a_stored_subject_other_than_the_user_is_refused_and_audited() {
    let fixture = fixture(DurableWorkerConfig::default(), None);
    let stored = ActingContext::new(AgentPrincipal::derived(APP, AGENT))
        .on_behalf_of(SubjectRef::human("mallory"));
    let finished = run_one(&fixture, new_run("alice").with_principal(stored)).await;

    assert_eq!(finished.state, RunState::Failed);
    assert!(
        finished
            .error
            .as_deref()
            .is_some_and(|error| error.contains("acts for human:mallory but its session belongs")),
        "{finished:?}"
    );
    assert!(fixture.policy.seen.lock().unwrap().is_empty(), "no agent ran");
    let records = fixture.audit.records();
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].action, SUBJECT_MISMATCH_CODE);
    assert_eq!(records[0].actor.on_behalf_of, Some(SubjectRef::human("mallory")));
    assert_eq!(records[0].ext["sessionSubject"], json!({ "kind": "human", "id": "alice" }));
}

#[tokio::test]
async fn a_delegation_rule_lets_a_service_run_act_for_a_user() {
    let delegation = StaticSubjectDelegation::new().allow_any(SubjectRef::service("crm-sync"));
    let fixture = fixture(DurableWorkerConfig::default(), Some(delegation));
    // The service that enqueued the run heads the chain, which names its kind.
    let stored = ActingContext::new(AgentPrincipal::derived(APP, AGENT))
        .on_behalf_of(SubjectRef::human("bob"))
        .delegated_by(SubjectRef::service("crm-sync"));
    let finished = run_one(&fixture, new_run("crm-sync").with_principal(stored.clone())).await;

    assert_eq!(finished.state, RunState::Completed, "{finished:?}");
    assert_eq!(*fixture.policy.seen.lock().unwrap(), vec![stored]);
    let records = tool_records(&fixture.audit);
    assert_eq!(records[0].actor.on_behalf_of, Some(SubjectRef::human("bob")));
    assert_eq!(records[0].actor.delegation_chain, vec![SubjectRef::service("crm-sync")]);
}

#[tokio::test]
async fn scheduled_runs_act_for_a_service() {
    let fixture = fixture(DurableWorkerConfig::default(), None);
    let schedules = Arc::new(InMemoryScheduleStore::new());
    let now = chrono::Utc::now();
    schedules
        .upsert(
            Schedule::new(
                "nightly-close",
                ScheduleSpec::Every(Duration::from_secs(3600)),
                new_run("nightly-close"),
                MissedFirePolicy::Skip,
            )
            .with_next_fire_at(now),
        )
        .await
        .unwrap();
    let scheduler = DurableScheduler::new(schedules, Arc::clone(&fixture.runs));
    assert_eq!(scheduler.tick().await.unwrap().enqueued, 1);
    let finished = fixture.worker.run_next().await.unwrap().expect("the scheduled run ran");

    assert_eq!(finished.state, RunState::Completed, "{finished:?}");
    let seen = fixture.policy.seen.lock().unwrap().clone();
    assert_eq!(
        seen,
        vec![
            ActingContext::new(AgentPrincipal::derived(APP, AGENT))
                .on_behalf_of(SubjectRef::service("nightly-close"))
        ]
    );
    let records = tool_records(&fixture.audit);
    assert_eq!(records[0].actor.on_behalf_of, Some(SubjectRef::service("nightly-close")));
}

#[tokio::test]
async fn interactive_runs_act_for_a_human_by_default() {
    let fixture = fixture(DurableWorkerConfig::default(), None);
    let finished = run_one(&fixture, new_run("alice")).await;

    assert_eq!(finished.state, RunState::Completed, "{finished:?}");
    let records = tool_records(&fixture.audit);
    assert_eq!(records[0].actor.on_behalf_of, Some(SubjectRef::human("alice")));
}
