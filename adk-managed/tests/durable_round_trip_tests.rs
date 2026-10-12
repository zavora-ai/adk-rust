//! Custom tool results and tool confirmations round-trip through the durable runtime.
//!
//! Regression tests for #753:
//!
//! | Defect | Test |
//! |--------|------|
//! | A custom tool result waited out the 300 s park and was then discarded; the model saw a placeholder | `a_custom_tool_result_reaches_the_model` |
//! | `user.tool_confirmation` never reached the agent | `an_allowed_confirmation_runs_the_held_call`, `a_denied_confirmation_reports_the_reason` |
//! | `FileManagedStateStore` was never attached | `the_runtime_writes_to_the_state_store_it_is_given` |
//!
//! Durable parking: a parked call survives a restart (a new runtime instance over the same
//! stores) and resumes when its result arrives; an answer recorded through another runtime
//! instance or through the approval service wakes it; an unanswered call expires through
//! the run store. Each scenario runs over in-memory stores shared by both instances and over
//! SQLite files reopened by the second instance; the PostgreSQL variants need
//! `DATABASE_URL` and are ignored by default.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::{
    ApprovalDecision, ApprovalPolicy, ApprovalService, ApproverSelector, InboxQuery, Llm,
    LlmRequest, LlmResponseStream, Part, StoreApprovalService, principal::SubjectRef,
};
use adk_managed::durable::{DurableConfig, ManagedStores};
use adk_managed::resolver::{ModelResolver, ResolverResult};
use adk_managed::state_store::{
    Durability, FileManagedStateStore, InMemoryManagedStateStore, ManagedStateStore,
    SessionServiceStateStore,
};
use adk_managed::testing::{ScriptedLlm, ScriptedToolCall, ScriptedTurn};
use adk_managed::types::{
    ConfirmationResult, ContentBlock, ManagedAgentDef, ModelRef, PermissionMode, PermissionPolicy,
    RuntimeError, SessionEvent, StopReason, ToolConfig, UserEvent,
};
use adk_managed::{DefaultManagedAgentRuntime, ManagedAgentRuntime, ManagedOwner, SessionHandle};
use adk_session::SessionService;
use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::{Value, json};

// ─── Model double ─────────────────────────────────────────────────────────────

/// A scripted model that records every request it receives.
struct RecordingLlm {
    scripted: ScriptedLlm,
    requests: Mutex<Vec<LlmRequest>>,
}

impl RecordingLlm {
    fn new(turns: Vec<ScriptedTurn>) -> Arc<Self> {
        Arc::new(Self {
            scripted: ScriptedLlm::new("recording", turns),
            requests: Mutex::default(),
        })
    }

    fn calls(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    /// The function responses the model was shown in its last request, by call ID.
    fn last_responses(&self) -> Vec<(Option<String>, Value)> {
        let requests = self.requests.lock().unwrap();
        let last = requests.last().expect("the model was called");
        last.contents
            .iter()
            .flat_map(|content| content.parts.iter())
            .filter_map(|part| match part {
                Part::FunctionResponse { function_response, id, .. } => {
                    Some((id.clone(), function_response.response.clone()))
                }
                _ => None,
            })
            .collect()
    }
}

#[async_trait]
impl Llm for RecordingLlm {
    fn name(&self) -> &str {
        "recording"
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        self.requests.lock().unwrap().push(request.clone());
        self.scripted.generate_content(request, stream).await
    }
}

struct Resolver(Arc<RecordingLlm>);

#[async_trait]
impl ModelResolver for Resolver {
    async fn resolve(&self, _model: &ModelRef) -> ResolverResult<Arc<dyn Llm>> {
        Ok(self.0.clone() as Arc<dyn Llm>)
    }
}

fn call(name: &str, id: &str, input: Value) -> ScriptedTurn {
    ScriptedTurn {
        text: None,
        tool_calls: vec![ScriptedToolCall { name: name.into(), input, id: Some(id.into()) }],
    }
}

fn say(text: &str) -> ScriptedTurn {
    ScriptedTurn { text: Some(text.into()), tool_calls: vec![] }
}

fn deploy_agent() -> ManagedAgentDef {
    ManagedAgentDef::new("deployer", ModelRef::Shorthand("gpt-5.4-mini".into()))
        .with_system("Deploy when asked.")
        .with_tools(vec![ToolConfig::Custom {
            name: "deploy".into(),
            description: Some("Deploy the application".into()),
            input_schema: json!({ "type": "object", "properties": { "target": { "type": "string" } } }),
        }])
}

fn guarded_bash_agent() -> ManagedAgentDef {
    ManagedAgentDef::new("operator", ModelRef::Shorthand("gpt-5.4-mini".into()))
        .with_tools(vec![ToolConfig::Bash {}])
        .with_permission_policy(PermissionPolicy {
            default: PermissionMode::AutoApprove,
            tools: [("bash".to_string(), PermissionMode::Prompt)].into(),
        })
}

fn text(message: &str) -> UserEvent {
    UserEvent::Message { content: vec![ContentBlock::Text { text: message.into() }] }
}

fn owner() -> ManagedOwner {
    ManagedOwner::new("ops-console", "user-7").unwrap()
}

fn fast() -> DurableConfig {
    DurableConfig::default()
        .with_poll_interval(Duration::from_millis(25))
        .with_lease_ttl(Duration::from_secs(5))
}

// ─── Stores ───────────────────────────────────────────────────────────────────

/// One stack of stores. Every `open` returns handles over the same stored data, as a
/// restarted or second process would see it.
#[async_trait]
trait Backend: Send + Sync {
    async fn open(&self) -> (Arc<dyn SessionService>, ManagedStores);
}

/// In-memory stores, shared by every instance of one test.
struct Shared {
    sessions: Arc<dyn SessionService>,
    stores: ManagedStores,
}

impl Shared {
    fn new() -> Self {
        Self {
            sessions: Arc::new(adk_session::InMemorySessionService::new()),
            stores: ManagedStores::in_memory(),
        }
    }
}

#[async_trait]
impl Backend for Shared {
    async fn open(&self) -> (Arc<dyn SessionService>, ManagedStores) {
        (self.sessions.clone(), self.stores.clone())
    }
}

/// SQLite files, reopened by each instance.
struct Sqlite {
    dir: std::path::PathBuf,
}

impl Sqlite {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "adk-managed-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }

    fn url(&self, file: &str) -> String {
        format!("sqlite://{}?mode=rwc", self.dir.join(file).display())
    }
}

impl Drop for Sqlite {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[async_trait]
impl Backend for Sqlite {
    async fn open(&self) -> (Arc<dyn SessionService>, ManagedStores) {
        let sessions =
            adk_session::SqliteSessionService::new(&self.url("sessions.db")).await.unwrap();
        sessions.migrate().await.unwrap();
        let state_sessions =
            adk_session::SqliteSessionService::new(&self.url("state.db")).await.unwrap();
        state_sessions.migrate().await.unwrap();
        let runs = adk_session::SqliteRunStore::new(&self.url("runs.db")).await.unwrap();
        runs.migrate().await.unwrap();
        let decisions =
            adk_session::SqliteApprovalStore::new(&self.url("approvals.db")).await.unwrap();
        decisions.migrate().await.unwrap();
        let state =
            SessionServiceStateStore::new(Arc::new(state_sessions), Durability::CrashDurable);
        (
            Arc::new(sessions),
            ManagedStores::new(Arc::new(state), Arc::new(runs), Arc::new(decisions)),
        )
    }
}

/// A PostgreSQL database named by `DATABASE_URL`.
struct Postgres {
    url: String,
}

#[async_trait]
impl Backend for Postgres {
    async fn open(&self) -> (Arc<dyn SessionService>, ManagedStores) {
        let sessions = adk_session::PostgresSessionService::new(&self.url).await.unwrap();
        sessions.migrate().await.unwrap();
        let runs = adk_session::PostgresRunStore::new(&self.url).await.unwrap();
        runs.migrate().await.unwrap();
        let decisions = adk_session::PostgresApprovalStore::new(&self.url).await.unwrap();
        decisions.migrate().await.unwrap();
        let sessions: Arc<dyn SessionService> = Arc::new(sessions);
        let state = SessionServiceStateStore::new(sessions.clone(), Durability::CrashDurable);
        (sessions, ManagedStores::new(Arc::new(state), Arc::new(runs), Arc::new(decisions)))
    }
}

fn postgres() -> Postgres {
    Postgres {
        url: std::env::var("DATABASE_URL").expect("set DATABASE_URL for the PostgreSQL variant"),
    }
}

async fn runtime(
    backend: &dyn Backend,
    llm: &Arc<RecordingLlm>,
    config: DurableConfig,
) -> DefaultManagedAgentRuntime {
    let (sessions, stores) = backend.open().await;
    DefaultManagedAgentRuntime::new(Arc::new(Resolver(llm.clone())), sessions)
        .with_stores(stores)
        .with_durable_config(config)
}

// ─── Event helpers ────────────────────────────────────────────────────────────

fn kind(event: &SessionEvent) -> &'static str {
    match event {
        SessionEvent::StatusRunning { .. } => "status.running",
        SessionEvent::Message { .. } => "agent.message",
        SessionEvent::ToolUse { .. } => "agent.tool_use",
        SessionEvent::CustomToolUse { .. } => "agent.custom_tool_use",
        SessionEvent::McpToolUse { .. } => "agent.mcp_tool_use",
        SessionEvent::StatusIdle { .. } => "status.idle",
        SessionEvent::Error { .. } => "error",
        _ => "unknown",
    }
}

/// Reads one turn: every event up to and including its `status.idle`.
async fn turn(
    stream: &mut BoxStream<'static, SessionEvent>,
) -> (Vec<&'static str>, Option<StopReason>) {
    let mut kinds = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), stream.next())
            .await
            .unwrap_or_else(|_| panic!("no status.idle within 10 s; saw {kinds:?}"))
            .expect("stream ended");
        kinds.push(kind(&event));
        if let SessionEvent::Error { message, .. } = &event {
            panic!("session error: {message}");
        }
        if let SessionEvent::StatusIdle { stop_reason, .. } = event {
            return (kinds, stop_reason);
        }
    }
}

fn requires_action(stop: Option<StopReason>) -> Vec<String> {
    match stop {
        Some(StopReason::RequiresAction { event_ids }) => event_ids,
        other => panic!("expected requires_action, got {other:?}"),
    }
}

async fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..400 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn start(
    runtime: &DefaultManagedAgentRuntime,
    def: ManagedAgentDef,
) -> (SessionHandle, BoxStream<'static, SessionEvent>) {
    let agent = runtime.create(def).await.unwrap();
    let session = runtime.start_session(&agent, &owner(), None).await.unwrap();
    let stream = runtime.stream_events(&session, None).await.unwrap();
    (session, stream)
}

fn deploy_result() -> UserEvent {
    UserEvent::CustomToolResult {
        custom_tool_use_id: "call_deploy".into(),
        content: vec![ContentBlock::Text { text: "deployed v2.1.0".into() }],
    }
}

// ─── #753 regressions ─────────────────────────────────────────────────────────

#[tokio::test]
async fn a_custom_tool_result_reaches_the_model() {
    let llm = RecordingLlm::new(vec![
        call("deploy", "call_deploy", json!({ "target": "prod" })),
        say("Deployed."),
    ]);
    let backend = Shared::new();
    let runtime = runtime(&backend, &llm, fast()).await;
    let (session, mut stream) = start(&runtime, deploy_agent()).await;

    runtime.send_event(&session, text("Deploy to prod")).await.unwrap();
    let (kinds, stop) = turn(&mut stream).await;
    assert_eq!(kinds, ["status.running", "agent.custom_tool_use", "status.idle"]);
    assert_eq!(requires_action(stop), ["call_deploy"]);
    assert_eq!(llm.calls(), 1, "the model is not asked to continue from a placeholder");

    // Answered well inside the old 300 s park: the loop reads its channel while parked.
    let answered = tokio::time::Instant::now();
    runtime.send_event(&session, deploy_result()).await.unwrap();
    let (kinds, stop) = turn(&mut stream).await;
    assert!(answered.elapsed() < Duration::from_secs(5));
    assert_eq!(kinds, ["status.running", "agent.message", "status.idle"]);
    assert!(matches!(stop, Some(StopReason::EndTurn)));

    assert_eq!(
        llm.last_responses(),
        vec![(Some("call_deploy".to_string()), json!({ "result": "deployed v2.1.0" }))],
        "the model sees the client's result in place of the pending response"
    );
}

#[tokio::test]
async fn an_unknown_or_repeated_answer_is_refused() {
    let llm = RecordingLlm::new(vec![call("deploy", "call_deploy", json!({})), say("Done.")]);
    let backend = Shared::new();
    let runtime = runtime(&backend, &llm, fast()).await;
    let (session, mut stream) = start(&runtime, deploy_agent()).await;

    runtime.send_event(&session, text("Deploy")).await.unwrap();
    turn(&mut stream).await;

    let message = runtime.send_event(&session, text("and again")).await;
    assert!(matches!(message, Err(RuntimeError::Conflict { .. })), "{message:?}");
    let unknown =
        UserEvent::CustomToolResult { custom_tool_use_id: "call_other".into(), content: vec![] };
    let refused = runtime.send_event(&session, unknown).await;
    assert!(matches!(refused, Err(RuntimeError::InvalidRequest { .. })), "{refused:?}");

    runtime.send_event(&session, deploy_result()).await.unwrap();
    turn(&mut stream).await;
    let repeated = runtime.send_event(&session, deploy_result()).await;
    assert!(matches!(repeated, Err(RuntimeError::InvalidRequest { .. })), "{repeated:?}");
}

#[tokio::test]
async fn an_allowed_confirmation_runs_the_held_call() {
    let llm = RecordingLlm::new(vec![
        call("bash", "call_bash", json!({ "command": "echo approved-run" })),
        say("It ran."),
    ]);
    let backend = Shared::new();
    let runtime = runtime(&backend, &llm, fast()).await;
    let (session, mut stream) = start(&runtime, guarded_bash_agent()).await;

    runtime.send_event(&session, text("run it")).await.unwrap();
    let (kinds, stop) = turn(&mut stream).await;
    assert_eq!(kinds, ["status.running", "agent.tool_use", "status.idle"]);
    assert_eq!(requires_action(stop), ["call_bash"]);
    assert_eq!(llm.calls(), 1, "the held call ends the turn");

    let allow = UserEvent::ToolConfirmation {
        tool_use_id: "call_bash".into(),
        result: ConfirmationResult::Allow,
        deny_message: None,
    };
    runtime.send_event(&session, allow).await.unwrap();
    let (kinds, _) = turn(&mut stream).await;
    assert_eq!(kinds, ["status.running", "agent.message", "status.idle"]);

    let responses = llm.last_responses();
    assert_eq!(responses.len(), 1, "{responses:?}");
    assert_eq!(responses[0].0.as_deref(), Some("call_bash"));
    assert!(
        responses[0].1["stdout"].as_str().unwrap_or_default().contains("approved-run"),
        "the approved call executed: {responses:?}"
    );
}

#[tokio::test]
async fn a_denied_confirmation_reports_the_reason() {
    let llm = RecordingLlm::new(vec![
        call("bash", "call_bash", json!({ "command": "echo should-not-run" })),
        say("Understood."),
    ]);
    let backend = Shared::new();
    let runtime = runtime(&backend, &llm, fast()).await;
    let (session, mut stream) = start(&runtime, guarded_bash_agent()).await;

    runtime.send_event(&session, text("run it")).await.unwrap();
    turn(&mut stream).await;
    let deny = UserEvent::ToolConfirmation {
        tool_use_id: "call_bash".into(),
        result: ConfirmationResult::Deny,
        deny_message: Some("not on a Friday".into()),
    };
    runtime.send_event(&session, deny).await.unwrap();
    turn(&mut stream).await;

    let responses = llm.last_responses();
    assert_eq!(responses.len(), 1, "{responses:?}");
    let response = responses[0].1.to_string();
    assert!(response.contains("not on a Friday"), "{response}");
    assert!(!response.contains("should-not-run\\n"), "the denied call did not run: {response}");
}

#[tokio::test]
async fn the_runtime_writes_to_the_state_store_it_is_given() {
    let root = std::env::temp_dir().join(format!("adk-managed-attach-{}", uuid::Uuid::new_v4()));
    let store = Arc::new(FileManagedStateStore::new(&root));
    let llm = RecordingLlm::new(vec![say("Hello.")]);
    let runtime = DefaultManagedAgentRuntime::new(
        Arc::new(Resolver(llm.clone())),
        Arc::new(adk_session::InMemorySessionService::new()),
    )
    .with_state_store(store.clone());
    assert_eq!(runtime.durability(), Durability::CrashDurable);

    let (session, mut stream) = start(&runtime, deploy_agent()).await;
    runtime.send_event(&session, text("hi")).await.unwrap();
    turn(&mut stream).await;

    let stored = store.load(&session.0).await.unwrap().expect("the session is in the file store");
    assert_eq!(stored.events.len(), 3, "running, message, idle");
    assert_eq!(stored.record.expect("the record is stored").agent.name, "deployer");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_default_runtime_reports_process_local_state() {
    let runtime = DefaultManagedAgentRuntime::new(
        Arc::new(Resolver(RecordingLlm::new(vec![]))),
        Arc::new(adk_session::InMemorySessionService::new()),
    );
    assert_eq!(runtime.durability(), Durability::ProcessLocal);
    assert_eq!(InMemoryManagedStateStore::new().durability(), Durability::ProcessLocal);
}

// ─── Durable parking ──────────────────────────────────────────────────────────

/// A parked custom tool call survives the runtime that parked it, and the result sent to
/// a new runtime instance over the same stores completes the turn.
async fn a_parked_call_survives_a_restart(backend: &dyn Backend) {
    let llm = RecordingLlm::new(vec![
        call("deploy", "call_deploy", json!({ "target": "prod" })),
        say("Deployed after the restart."),
    ]);
    let session = {
        let first = runtime(backend, &llm, fast()).await;
        let (session, mut stream) = start(&first, deploy_agent()).await;
        first.send_event(&session, text("Deploy to prod")).await.unwrap();
        let (_, stop) = turn(&mut stream).await;
        assert_eq!(requires_action(stop), ["call_deploy"]);
        session
        // The first instance and its session loop end here.
    };
    tokio::time::sleep(Duration::from_millis(50)).await;

    let second = runtime(backend, &llm, fast()).await;
    let mut stream = second.stream_events(&session, Some(2)).await.expect("the session is rebuilt");
    second.send_event(&session, deploy_result()).await.unwrap();
    let (kinds, stop) = turn(&mut stream).await;
    assert_eq!(kinds, ["status.running", "agent.message", "status.idle"]);
    assert!(matches!(stop, Some(StopReason::EndTurn)));
    assert_eq!(llm.calls(), 2);
    assert_eq!(
        llm.last_responses(),
        vec![(Some("call_deploy".to_string()), json!({ "result": "deployed v2.1.0" }))]
    );

    // The rebuilt session continues its sequence and replays the first instance's events.
    let mut replay = second.stream_events(&session, Some(0)).await.unwrap();
    let first = tokio::time::timeout(Duration::from_secs(2), replay.next()).await.unwrap().unwrap();
    assert!(matches!(first, SessionEvent::CustomToolUse { seq: 1, .. }), "{first:?}");
}

#[tokio::test]
async fn a_parked_call_survives_a_restart_in_memory() {
    a_parked_call_survives_a_restart(&Shared::new()).await;
}

#[tokio::test]
async fn a_parked_call_survives_a_restart_on_sqlite() {
    a_parked_call_survives_a_restart(&Sqlite::new("restart")).await;
}

#[tokio::test]
#[ignore = "needs DATABASE_URL"]
async fn a_parked_call_survives_a_restart_on_postgres() {
    a_parked_call_survives_a_restart(&postgres()).await;
}

/// A result sent to a second runtime instance, while the first still hosts the session,
/// wakes the parked run; one of them completes the turn exactly once.
async fn a_result_sent_to_another_replica_wakes_the_run(backend: &dyn Backend) {
    let llm = RecordingLlm::new(vec![
        call("deploy", "call_deploy", json!({ "target": "prod" })),
        say("Deployed via the other replica."),
    ]);
    let replica_a = runtime(backend, &llm, fast().with_worker_id("replica-a")).await;
    let replica_b = runtime(backend, &llm, fast().with_worker_id("replica-b")).await;
    let (session, mut stream) = start(&replica_a, deploy_agent()).await;
    replica_a.send_event(&session, text("Deploy to prod")).await.unwrap();
    turn(&mut stream).await;

    replica_b.send_event(&session, deploy_result()).await.unwrap();
    wait_for("the resumed turn", || llm.calls() == 2).await;
    let (sessions, stores) = backend.open().await;
    let identity = adk_session::GetRequest {
        app_name: "ops-console".into(),
        user_id: "user-7".into(),
        session_id: session.0.clone(),
        num_recent_events: None,
        after: None,
    };
    let mut finished = false;
    for _ in 0..200 {
        let state = stores.state.load(&session.0).await.unwrap().unwrap();
        if state.durable_run_id.is_none() && state.pending.is_empty() {
            finished = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(finished, "the run completed and released the session");
    let events = sessions.get(identity).await.unwrap().events().all();
    let finals: Vec<_> = events
        .iter()
        .filter(|event| {
            event.llm_response.content.as_ref().is_some_and(|content| {
                content
                    .parts
                    .iter()
                    .any(|part| part.text() == Some("Deployed via the other replica."))
            })
        })
        .collect();
    assert_eq!(finals.len(), 1, "the turn completed exactly once");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(llm.calls(), 2, "no replica ran the turn twice");
}

#[tokio::test]
async fn a_result_sent_to_another_replica_wakes_the_run_in_memory() {
    a_result_sent_to_another_replica_wakes_the_run(&Shared::new()).await;
}

#[tokio::test]
async fn a_result_sent_to_another_replica_wakes_the_run_on_sqlite() {
    a_result_sent_to_another_replica_wakes_the_run(&Sqlite::new("replica")).await;
}

#[tokio::test]
#[ignore = "needs DATABASE_URL"]
async fn a_result_sent_to_another_replica_wakes_the_run_on_postgres() {
    a_result_sent_to_another_replica_wakes_the_run(&postgres()).await;
}

/// An unanswered custom tool call expires through the run store: the model is told the
/// result did not arrive, and the turn ends.
#[tokio::test]
async fn an_unanswered_call_expires() {
    let llm =
        RecordingLlm::new(vec![call("deploy", "call_deploy", json!({})), say("It timed out.")]);
    let backend = Shared::new();
    let config = fast().with_pending_expiry(Some(Duration::from_millis(200)));
    let runtime = runtime(&backend, &llm, config).await;
    let (session, mut stream) = start(&runtime, deploy_agent()).await;

    runtime.send_event(&session, text("Deploy")).await.unwrap();
    turn(&mut stream).await;
    let (kinds, _) = turn(&mut stream).await;
    assert_eq!(kinds, ["status.running", "agent.message", "status.idle"]);
    let responses = llm.last_responses();
    assert_eq!(responses[0].1["status"], "expired", "{responses:?}");

    let late = runtime.send_event(&session, deploy_result()).await;
    assert!(late.is_err(), "a result after expiry is refused");
}

/// A confirmation decided through the approval service wakes the parked run without any
/// runtime call.
#[tokio::test]
async fn an_approval_service_decision_wakes_the_run() {
    let llm = RecordingLlm::new(vec![
        call("bash", "call_bash", json!({ "command": "echo approved-by-ops" })),
        say("Ran after approval."),
    ]);
    let backend = Shared::new();
    let (sessions, stores) = backend.open().await;
    let service = Arc::new(
        StoreApprovalService::new(stores.decisions.clone())
            .with_run_store(stores.runs.clone())
            .with_router(ApprovalPolicy::new([ApproverSelector::group("ops")])),
    );
    let runtime = DefaultManagedAgentRuntime::new(Arc::new(Resolver(llm.clone())), sessions)
        .with_stores(stores.with_approval_service(service.clone()))
        .with_durable_config(fast());
    let (session, mut stream) = start(&runtime, guarded_bash_agent()).await;

    runtime.send_event(&session, text("run it")).await.unwrap();
    assert_eq!(requires_action(turn(&mut stream).await.1), ["call_bash"]);
    let direct = UserEvent::ToolConfirmation {
        tool_use_id: "call_bash".into(),
        result: ConfirmationResult::Allow,
        deny_message: None,
    };
    assert!(runtime.send_event(&session, direct).await.is_err(), "the service decides");

    let inbox = service.inbox(InboxQuery::for_group("ops")).await.unwrap();
    assert_eq!(inbox.items.len(), 1);
    let request = &inbox.items[0];
    assert!(request.run_id.is_some(), "the request names the parked run");
    service
        .decide(
            &request.id,
            request.version,
            ApprovalDecision::approve(SubjectRef::human("ana")).with_groups(["ops"]),
        )
        .await
        .unwrap();

    let (kinds, _) = turn(&mut stream).await;
    assert_eq!(kinds, ["status.running", "agent.message", "status.idle"]);
    let responses = llm.last_responses();
    assert!(responses[0].1["stdout"].as_str().unwrap_or_default().contains("approved-by-ops"));
}
