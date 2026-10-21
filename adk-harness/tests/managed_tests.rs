//! The managed form (spec 3.1 §7.18, H9): the managed runtime builds a harness from a
//! `HarnessDef`, runs it in its supervised session loop, parks held confirmations and custom
//! tool calls on durable runs answered through its approval store, runs an approved call
//! once under the action ledger, and replays the session to an `adk-enterprise` client.

#![cfg(feature = "managed")]

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::{
    AdkError, AdkIdentity, Event, InMemoryActionLedger, Llm, Part, Result, async_trait,
};
use adk_harness::HarnessAgent;
use adk_harness::packs::managed::{HarnessDef, HarnessResolver, ShellPolicy};
use adk_managed::durable::DurableConfig;
use adk_managed::resolver::{ResolverError, ResolverResult};
use adk_managed::testing::{ScriptedLlm, ScriptedToolCall, ScriptedTurn};
use adk_managed::types::{
    ConfirmationResult, ContentBlock, ManagedAgentDef, ModelRef, RuntimeError, SessionEvent,
    StopReason, UserEvent,
};
use adk_managed::{
    DefaultManagedAgentRuntime, ManagedAgentRuntime, ManagedOwner, ManagedStores, ModelResolver,
    SessionHandle,
};
use adk_session::{
    AppendEventRequest, CreateRequest, DeleteRequest, GetRequest, InMemorySessionService,
    ListRequest, Session, SessionService,
};
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::Deserialize;
use serde_json::{Value, json};

// ─── Hosting ─────────────────────────────────────────────────────────────────

/// Resolves every model reference to one scripted model.
struct Scripted(Arc<ScriptedLlm>);

#[async_trait]
impl ModelResolver for Scripted {
    async fn resolve(&self, _model: &ModelRef) -> ResolverResult<Arc<dyn Llm>> {
        Ok(self.0.clone() as Arc<dyn Llm>)
    }
}

/// Sessions whose next save of a `bash` result fails once armed: the call ran and its
/// result never reached the session.
struct CrashingSessions {
    inner: InMemorySessionService,
    armed: AtomicBool,
}

impl CrashingSessions {
    fn crashes(&self, event: &Event) -> bool {
        let saves_bash_result = event.llm_response.content.as_ref().is_some_and(|content| {
            content.parts.iter().any(|part| {
                matches!(part, Part::FunctionResponse { function_response, .. }
                    if function_response.name == "bash"
                        && !adk_core::is_confirmation_pending(&function_response.response))
            })
        });
        saves_bash_result && self.armed.swap(false, Ordering::SeqCst)
    }
}

#[async_trait]
impl SessionService for CrashingSessions {
    async fn create(&self, req: CreateRequest) -> Result<Box<dyn Session>> {
        self.inner.create(req).await
    }
    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>> {
        self.inner.get(req).await
    }
    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>> {
        self.inner.list(req).await
    }
    async fn delete(&self, req: DeleteRequest) -> Result<()> {
        self.inner.delete(req).await
    }
    async fn append_event(&self, session_id: &str, event: Event) -> Result<()> {
        if self.crashes(&event) {
            return Err(AdkError::session("simulated crash before the result is saved"));
        }
        self.inner.append_event(session_id, event).await
    }
    async fn append_event_for_identity(&self, req: AppendEventRequest) -> Result<()> {
        if self.crashes(&req.event) {
            return Err(AdkError::session("simulated crash before the result is saved"));
        }
        self.inner.append_event_for_identity(req).await
    }
    async fn get_for_identity(&self, identity: &AdkIdentity) -> Result<Box<dyn Session>> {
        self.inner.get_for_identity(identity).await
    }
}

/// What every runtime instance of a deployment shares: sessions, managed stores, the
/// action ledger and the workspaces.
struct Deployment {
    workspaces: tempfile::TempDir,
    sessions: Arc<CrashingSessions>,
    stores: ManagedStores,
    ledger: Arc<InMemoryActionLedger>,
}

impl Deployment {
    fn new() -> Self {
        Self {
            workspaces: tempfile::tempdir().unwrap(),
            sessions: Arc::new(CrashingSessions {
                inner: InMemorySessionService::new(),
                armed: AtomicBool::new(false),
            }),
            stores: ManagedStores::in_memory(),
            ledger: Arc::new(InMemoryActionLedger::new()),
        }
    }

    fn resolver(&self) -> HarnessResolver {
        let ledger = self.ledger.clone();
        HarnessResolver::new(self.workspaces.path())
            .allow_host_shell(true)
            .configure(move |builder, _def| builder.action_ledger(ledger.clone()))
    }

    /// A runtime instance whose model replies with `turns`.
    fn instance(&self, turns: Vec<ScriptedTurn>) -> (DefaultManagedAgentRuntime, Arc<ScriptedLlm>) {
        let llm = Arc::new(ScriptedLlm::new("scripted", turns));
        let runtime = DefaultManagedAgentRuntime::new(
            Arc::new(Scripted(llm.clone())),
            self.sessions.clone() as Arc<dyn SessionService>,
        )
        .with_stores(self.stores.clone())
        .with_durable_config(DurableConfig::default().with_poll_interval(Duration::from_millis(50)))
        .with_agent_resolver(Arc::new(self.resolver()));
        (runtime, llm)
    }

    fn path(&self, agent: &str, relative: &str) -> PathBuf {
        self.workspaces.path().join(agent).join(relative)
    }

    fn read(&self, agent: &str, relative: &str) -> Option<String> {
        std::fs::read_to_string(self.path(agent, relative)).ok()
    }

    /// The conversation of `session`, as the session service holds it.
    async fn conversation(&self, session: &SessionHandle) -> Vec<Event> {
        let request = GetRequest {
            app_name: "app".into(),
            user_id: "ana".into(),
            session_id: session.0.clone(),
            num_recent_events: None,
            after: None,
        };
        self.sessions.get(request).await.unwrap().events().all()
    }
}

/// The latest response to `tool` in `events`.
fn last_answer(events: &[Event], tool: &str) -> Option<Value> {
    events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, .. } if function_response.name == tool => {
                Some(function_response.response.clone())
            }
            _ => None,
        })
        .next_back()
}

fn owner() -> ManagedOwner {
    ManagedOwner::new("app", "ana").unwrap()
}

fn model() -> ModelRef {
    ModelRef::Shorthand("gemini-3.7-flash".into())
}

fn call(name: &str, input: Value, id: &str) -> ScriptedTurn {
    ScriptedTurn {
        text: None,
        tool_calls: vec![ScriptedToolCall { name: name.into(), input, id: Some(id.into()) }],
    }
}

fn text(reply: &str) -> ScriptedTurn {
    ScriptedTurn { text: Some(reply.into()), tool_calls: vec![] }
}

fn message(text: &str) -> UserEvent {
    UserEvent::Message { content: vec![ContentBlock::Text { text: text.into() }] }
}

fn allow(id: &str) -> UserEvent {
    UserEvent::ToolConfirmation {
        tool_use_id: id.into(),
        result: ConfirmationResult::Allow,
        deny_message: None,
    }
}

const LOG_LINE: &str = "printf 'ran\\n' >> log.txt";

/// The events up to and including the next `status.idle`.
async fn until_idle(stream: &mut BoxStream<'static, SessionEvent>) -> Vec<SessionEvent> {
    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(20), stream.next())
            .await
            .unwrap_or_else(|_| panic!("no status.idle after {:?}", labels(&events)))
            .expect("stream ended");
        let idle = matches!(event, SessionEvent::StatusIdle { .. });
        events.push(event);
        if idle {
            return events;
        }
    }
}

fn label(event: &SessionEvent) -> String {
    match event {
        SessionEvent::StatusRunning { .. } => "status.running".into(),
        SessionEvent::Message { .. } => "agent.message".into(),
        SessionEvent::ToolUse { .. } => "agent.tool_use".into(),
        SessionEvent::CustomToolUse { .. } => "agent.custom_tool_use".into(),
        SessionEvent::McpToolUse { .. } => "agent.mcp_tool_use".into(),
        SessionEvent::StatusIdle { stop_reason, .. } => match stop_reason {
            Some(StopReason::RequiresAction { .. }) => "status.idle:requires_action".into(),
            Some(StopReason::EndTurn) => "status.idle:end_turn".into(),
            Some(StopReason::MaxTokens) => "status.idle:max_tokens".into(),
            Some(_) | None => "status.idle".into(),
        },
        SessionEvent::Error { code, message, .. } => format!("error:{code}:{message}"),
        _ => "unknown".into(),
    }
}

fn labels(events: &[SessionEvent]) -> Vec<String> {
    events.iter().map(label).collect()
}

fn waits_on(events: &[SessionEvent]) -> Vec<String> {
    match events.last() {
        Some(SessionEvent::StatusIdle {
            stop_reason: Some(StopReason::RequiresAction { event_ids }),
            ..
        }) => event_ids.clone(),
        _ => Vec::new(),
    }
}

// ─── Golden fixtures ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct Fixture {
    name: String,
    harness_def: HarnessDef,
    scripted_model: ScriptedModel,
    scenario: Vec<UserEvent>,
    assertions: Assertions,
}

#[derive(Deserialize)]
struct ScriptedModel {
    turns: Vec<ScriptedTurn>,
}

#[derive(Deserialize)]
struct Assertions {
    exact_sequence: Vec<String>,
    files: BTreeMap<String, Option<String>>,
}

/// Runs a fixture through the managed runtime: the definition is a `HarnessDef`, built by
/// the `HarnessResolver` over a `ScriptedLlm`, and each scenario event is sent once the
/// previous turn ended with `status.idle`.
async fn run_fixture(file: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/managed").join(file);
    let fixture: Fixture = serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let deployment = Deployment::new();
    let (runtime, llm) = deployment.instance(fixture.scripted_model.turns.clone());
    let name = fixture.harness_def.agent.name.clone();
    let agent = runtime.create(fixture.harness_def.into()).await.unwrap();
    let session = runtime.start_session(&agent, &owner(), None).await.unwrap();
    let mut stream = runtime.stream_events(&session, None).await.unwrap();
    let mut seen = Vec::new();
    for event in fixture.scenario {
        runtime.send_event(&session, event).await.unwrap();
        seen.extend(labels(&until_idle(&mut stream).await));
    }
    assert_eq!(seen, fixture.assertions.exact_sequence, "{}", fixture.name);
    for (file, content) in fixture.assertions.files {
        assert_eq!(deployment.read(&name, &file), content, "{}: {file}", fixture.name);
    }
    assert_eq!(llm.remaining(), 0, "{}: the script is used up", fixture.name);
}

#[tokio::test]
async fn h1_a_held_write_runs_once_after_approval() {
    run_fixture("h1_held_write.json").await;
}

#[tokio::test]
async fn h2_the_definitions_mode_governs() {
    run_fixture("h2_plan_mode.json").await;
}

#[tokio::test]
async fn h3_a_custom_tool_is_confirmed_then_parks_for_its_client_result() {
    run_fixture("h3_custom_tool.json").await;
}

// ─── Exactly once ────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_held_bash_approved_on_a_restarted_instance_runs_once() {
    let deployment = Deployment::new();
    let def: ManagedAgentDef =
        HarnessDef::new("ops", model()).with_shell(ShellPolicy::host()).into();

    // Instance A parks the turn on the held call, then its process ends.
    let (a, _) = deployment.instance(vec![call("bash", json!({ "command": LOG_LINE }), "tc_bash")]);
    let agent = a.create(def).await.unwrap();
    let session = a.start_session(&agent, &owner(), None).await.unwrap();
    let mut stream = a.stream_events(&session, None).await.unwrap();
    a.send_event(&session, message("log a line")).await.unwrap();
    let parked = until_idle(&mut stream).await;
    assert_eq!(waits_on(&parked), ["tc_bash"], "{:?}", labels(&parked));
    assert_eq!(deployment.read("ops", "log.txt"), None, "held, not run");
    drop((stream, a));
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Instance B rebuilds the session from the stores and takes the client's approval.
    let (b, _) = deployment.instance(vec![text("Logged.")]);
    let mut stream = b.stream_events(&session, None).await.unwrap();
    b.send_event(&session, allow("tc_bash")).await.unwrap();
    let resumed = until_idle(&mut stream).await;
    assert_eq!(labels(&resumed), ["status.running", "agent.message", "status.idle:end_turn"]);
    assert_eq!(deployment.read("ops", "log.txt").as_deref(), Some("ran\n"));

    // A repeated answer is refused: the session waits on nothing.
    let error = b.send_event(&session, allow("tc_bash")).await.unwrap_err();
    assert!(matches!(error, RuntimeError::InvalidRequest { .. }), "{error:?}");
    assert_eq!(deployment.read("ops", "log.txt").as_deref(), Some("ran\n"));
}

#[tokio::test]
async fn a_bash_that_ran_before_its_result_was_saved_is_answered_from_the_ledger() {
    let deployment = Deployment::new();
    let def: ManagedAgentDef =
        HarnessDef::new("ops", model()).with_shell(ShellPolicy::host()).into();
    let (runtime, _) = deployment.instance(vec![
        call("bash", json!({ "command": LOG_LINE }), "tc_bash"),
        text("It ran once already."),
    ]);
    let agent = runtime.create(def).await.unwrap();
    let session = runtime.start_session(&agent, &owner(), None).await.unwrap();
    let mut stream = runtime.stream_events(&session, None).await.unwrap();
    runtime.send_event(&session, message("log a line")).await.unwrap();
    assert_eq!(waits_on(&until_idle(&mut stream).await), ["tc_bash"]);

    // The approved call runs; its result never reaches the session.
    deployment.sessions.armed.store(true, Ordering::SeqCst);
    runtime.send_event(&session, allow("tc_bash")).await.unwrap();
    let crashed = labels(&until_idle(&mut stream).await);
    assert!(crashed[1].contains("simulated crash"), "{crashed:?}");
    assert_eq!(deployment.read("ops", "log.txt").as_deref(), Some("ran\n"));

    // The next turn answers the call from its ledger record instead of running it.
    runtime.send_event(&session, message("did it run?")).await.unwrap();
    let next = until_idle(&mut stream).await;
    assert_eq!(labels(&next), ["status.running", "agent.message", "status.idle:end_turn"]);
    assert_eq!(deployment.read("ops", "log.txt").as_deref(), Some("ran\n"), "never run twice");
    let events = deployment.conversation(&session).await;
    assert!(adk_core::held_tool_calls(&events, "ops", "").is_empty(), "the call is not stranded");
    assert_eq!(last_answer(&events, "bash").unwrap()["status"], "already_succeeded");
}

/// Approvals whose decision is lost once armed: the turn takes it and stops before the
/// call begins.
#[derive(Debug)]
struct LosingApprovals {
    inner: adk_core::InMemoryApprovalStore,
    armed: AtomicBool,
}

#[async_trait]
impl adk_core::ApprovalStore for LosingApprovals {
    async fn record_pending(
        &self,
        scope: &adk_core::ApprovalScope,
        request: &adk_core::ToolConfirmationRequest,
    ) -> Result<()> {
        self.inner.record_pending(scope, request).await
    }
    async fn pending(
        &self,
        scope: &adk_core::ApprovalScope,
    ) -> Result<Vec<adk_core::PendingApproval>> {
        self.inner.pending(scope).await
    }
    async fn decide(
        &self,
        scope: &adk_core::ApprovalScope,
        fingerprint: &str,
        approval: adk_core::ToolApproval,
    ) -> Result<()> {
        self.inner.decide(scope, fingerprint, approval).await
    }
    async fn take_decision(
        &self,
        scope: &adk_core::ApprovalScope,
        fingerprint: &str,
    ) -> Result<Option<adk_core::ToolConfirmationDecision>> {
        self.inner.take_decision(scope, fingerprint).await
    }
    async fn has_decision(
        &self,
        scope: &adk_core::ApprovalScope,
        fingerprint: &str,
    ) -> Result<bool> {
        self.inner.has_decision(scope, fingerprint).await
    }
    async fn take_approval(
        &self,
        scope: &adk_core::ApprovalScope,
        fingerprint: &str,
    ) -> Result<Option<adk_core::ToolApproval>> {
        let taken = self.inner.take_approval(scope, fingerprint).await?;
        if taken.is_some() && self.armed.swap(false, Ordering::SeqCst) {
            return Err(AdkError::session("simulated crash after the decision was taken"));
        }
        Ok(taken)
    }
}

#[tokio::test]
async fn a_held_call_whose_decision_was_lost_before_it_began_is_held_again() {
    let mut deployment = Deployment::new();
    let approvals = Arc::new(LosingApprovals {
        inner: adk_core::InMemoryApprovalStore::new(),
        armed: AtomicBool::new(false),
    });
    deployment.stores.decisions = approvals.clone();
    let def: ManagedAgentDef =
        HarnessDef::new("ops", model()).with_shell(ShellPolicy::host()).into();
    let (runtime, _) = deployment.instance(vec![
        call("bash", json!({ "command": LOG_LINE }), "tc_bash"),
        text("Waiting for your approval."),
        text("Logged."),
    ]);
    let agent = runtime.create(def).await.unwrap();
    let session = runtime.start_session(&agent, &owner(), None).await.unwrap();
    let mut stream = runtime.stream_events(&session, None).await.unwrap();
    runtime.send_event(&session, message("log a line")).await.unwrap();
    assert_eq!(waits_on(&until_idle(&mut stream).await), ["tc_bash"]);

    approvals.armed.store(true, Ordering::SeqCst);
    runtime.send_event(&session, allow("tc_bash")).await.unwrap();
    let lost = labels(&until_idle(&mut stream).await);
    assert!(lost[1].contains("simulated crash"), "{lost:?}");
    assert_eq!(deployment.read("ops", "log.txt"), None, "it never began");

    // The next turn holds the call again and parks on it; the client's approval runs it once.
    runtime.send_event(&session, message("go on")).await.unwrap();
    let held = until_idle(&mut stream).await;
    assert_eq!(labels(&held), ["status.running", "agent.message", "status.idle:requires_action"]);
    assert_eq!(waits_on(&held), ["tc_bash"]);
    runtime.send_event(&session, allow("tc_bash")).await.unwrap();
    let ran = until_idle(&mut stream).await;
    assert_eq!(labels(&ran), ["status.running", "agent.message", "status.idle:end_turn"]);
    assert_eq!(deployment.read("ops", "log.txt").as_deref(), Some("ran\n"));
}

// ─── The same harness value (H9) ─────────────────────────────────────────────

/// Resolves no model: a definition of an added harness must not need one.
struct NoModels;

#[async_trait]
impl ModelResolver for NoModels {
    async fn resolve(&self, model: &ModelRef) -> ResolverResult<Arc<dyn Llm>> {
        Err(ResolverError::UnknownProvider { name: format!("{model:?}") })
    }
}

#[tokio::test]
async fn an_added_harness_value_serves_the_definition_named_like_it() {
    let workspace = tempfile::tempdir().unwrap();
    let harness = HarnessAgent::builder("ops")
        .model(Arc::new(ScriptedLlm::new(
            "scripted",
            vec![call("bash", json!({ "command": LOG_LINE }), "tc_bash"), text("Logged.")],
        )))
        .workspace(adk_devtools::Workspace::new(workspace.path()).allow_bash(true))
        .project_instructions(adk_harness::InstructionsConfig {
            user_file: None,
            ..adk_harness::InstructionsConfig::default()
        })
        .build()
        .unwrap();
    let resolver = HarnessResolver::new(tempfile::tempdir().unwrap().path()).with_harness(&harness);
    let runtime = DefaultManagedAgentRuntime::new(
        Arc::new(NoModels),
        Arc::new(InMemorySessionService::new()),
    )
    .with_agent_resolver(Arc::new(resolver));

    let agent = runtime
        .create(ManagedAgentDef::new("ops", ModelRef::Shorthand("never-resolved".into())))
        .await
        .unwrap();
    let session = runtime.start_session(&agent, &owner(), None).await.unwrap();
    let mut stream = runtime.stream_events(&session, None).await.unwrap();
    runtime.send_event(&session, message("log a line")).await.unwrap();
    assert_eq!(waits_on(&until_idle(&mut stream).await), ["tc_bash"]);
    runtime.send_event(&session, allow("tc_bash")).await.unwrap();
    until_idle(&mut stream).await;
    let log = std::fs::read_to_string(workspace.path().join("log.txt")).unwrap();
    assert_eq!(log, "ran\n", "the added harness's own workspace and model ran the turn");

    // A definition of an added harness carries no harness block of its own.
    let error = runtime.create(HarnessDef::new("ops", model()).into()).await.unwrap_err();
    assert!(matches!(error, RuntimeError::InvalidRequest { .. }), "{error:?}");
}

#[tokio::test]
async fn a_definition_field_the_harness_does_not_take_is_refused() {
    let deployment = Deployment::new();
    let (runtime, _) = deployment.instance(Vec::new());
    let mut bash = HarnessDef::new("a", model());
    bash.agent.tools.push(adk_managed::types::ToolConfig::Bash {});
    let host_refused = HarnessResolver::new(deployment.workspaces.path());
    for (def, expected) in [
        (bash, "tool `bash`"),
        (HarnessDef::new("b", model()).with_pack("managed"), "pack `managed`"),
        (HarnessDef::new("c", model()).with_skills_root("../escape"), "skills root"),
    ] {
        let error = runtime.create(def.into()).await.expect_err("refused");
        assert!(error.to_string().contains(expected), "{error}");
    }
    let host = HarnessDef::new("d", model()).with_shell(ShellPolicy::host());
    let error =
        host_refused.build(&host, Arc::new(ScriptedLlm::new("s", Vec::<ScriptedTurn>::new())));
    assert!(error.unwrap_err().to_string().contains("shell `host`"));
}

// ─── Replay to an adk-enterprise client ──────────────────────────────────────

/// The platform's HTTP surface over the runtime, as a self-hosted deployment serves it:
/// `POST /sessions/{id}/events` and `GET /sessions/{id}/events/stream` with
/// `Last-Event-ID`. The first stream connection ends after the turn parks, so the client
/// must reconnect.
#[derive(Clone)]
struct Platform {
    runtime: Arc<DefaultManagedAgentRuntime>,
    connections: Arc<Mutex<Vec<Option<String>>>>,
}

async fn post_event(
    axum::extract::State(platform): axum::extract::State<Platform>,
    axum::extract::Path(id): axum::extract::Path<String>,
    axum::Json(event): axum::Json<UserEvent>,
) -> axum::http::StatusCode {
    match platform.runtime.send_event(&SessionHandle(id), event).await {
        Ok(()) => axum::http::StatusCode::ACCEPTED,
        Err(_) => axum::http::StatusCode::CONFLICT,
    }
}

async fn event_stream(
    axum::extract::State(platform): axum::extract::State<Platform>,
    axum::extract::Path(id): axum::extract::Path<String>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let last = headers.get("last-event-id").and_then(|value| value.to_str().ok()).map(String::from);
    let first = {
        let mut connections = platform.connections.lock().unwrap();
        connections.push(last.clone());
        connections.len() == 1
    };
    let from_seq = last.as_deref().and_then(|value| value.parse::<u64>().ok());
    let events = platform.runtime.stream_events(&SessionHandle(id), from_seq).await.unwrap();
    let events: BoxStream<'static, SessionEvent> = if first {
        Box::pin(events.scan(false, |ended, event| {
            let next = (!*ended).then(|| {
                *ended = matches!(event, SessionEvent::StatusIdle { .. });
                event
            });
            futures::future::ready(next)
        }))
    } else {
        events
    };
    let frames = events.map(|event| {
        let value = serde_json::to_value(&event).unwrap();
        let kind = value["type"].as_str().unwrap_or_default().to_string();
        let seq = adk_managed::get_seq(&event);
        Ok::<_, Infallible>(format!("id: {seq}\nevent: {kind}\ndata: {value}\n\n"))
    });
    axum::response::Response::builder()
        .header("content-type", "text/event-stream")
        .body(axum::body::Body::from_stream(frames))
        .unwrap()
}

fn enterprise_seq(event: &adk_enterprise::SessionEvent) -> (u64, String) {
    use adk_enterprise::SessionEvent as E;
    match event {
        E::Message { seq, .. } => (*seq, "agent.message".into()),
        E::ToolUse { seq, .. } => (*seq, "agent.tool_use".into()),
        E::CustomToolUse { seq, .. } => (*seq, "agent.custom_tool_use".into()),
        E::McpToolUse { seq, .. } => (*seq, "agent.mcp_tool_use".into()),
        E::StatusRunning { seq } => (*seq, "status.running".into()),
        E::StatusIdle { seq, stop_reason, .. } => match stop_reason {
            Some(adk_enterprise::StopReason::RequiresAction { .. }) => {
                (*seq, "status.idle:requires_action".into())
            }
            Some(adk_enterprise::StopReason::EndTurn) => (*seq, "status.idle:end_turn".into()),
            _ => (*seq, "status.idle".into()),
        },
        E::Error { seq, message, .. } => (*seq, format!("error:{message}")),
        _ => (u64::MAX, "unknown".into()),
    }
}

async fn enterprise_until_idle(stream: &mut adk_enterprise::EventStream) -> Vec<(u64, String)> {
    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(20), stream.next())
            .await
            .unwrap_or_else(|_| panic!("no status.idle after {events:?}"))
            .expect("stream ended")
            .expect("stream error");
        let (seq, kind) = enterprise_seq(&event);
        let idle = kind.starts_with("status.idle");
        events.push((seq, kind));
        if idle {
            return events;
        }
    }
}

#[tokio::test]
async fn an_enterprise_client_replays_a_parked_harness_session_after_a_reconnect() {
    let deployment = Deployment::new();
    let (runtime, _) = deployment.instance(vec![
        call("write_file", json!({ "path": "notes.txt", "content": "hello\n" }), "tc_write"),
        text("Wrote notes.txt."),
    ]);
    let runtime = Arc::new(runtime);
    let def = HarnessDef::new("builder", model()).with_shell(ShellPolicy::none());
    let agent = runtime.create(def.into()).await.unwrap();
    let session = runtime.start_session(&agent, &owner(), None).await.unwrap();

    let platform = Platform { runtime: runtime.clone(), connections: Arc::default() };
    let app = axum::Router::new()
        .route("/sessions/{id}/events", axum::routing::post(post_event))
        .route("/sessions/{id}/events/stream", axum::routing::get(event_stream))
        .with_state(platform.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });

    let client =
        adk_enterprise::EnterpriseClient::self_hosted("local", format!("http://{address}"))
            .unwrap();
    let mut events = client.stream_events(&session.0).await.unwrap();
    client.send_message(&session.0, "Write hello to notes.txt").await.unwrap();
    let parked = enterprise_until_idle(&mut events).await;
    let kinds: Vec<&str> = parked.iter().map(|(_, kind)| kind.as_str()).collect();
    assert_eq!(kinds, ["status.running", "agent.tool_use", "status.idle:requires_action"]);

    // The client approves while its stream is down, and the turn runs to its end.
    let mut watch = runtime.stream_events(&session, None).await.unwrap();
    client.allow_tool(&session.0, "tc_write").await.unwrap();
    until_idle(&mut watch).await;
    assert_eq!(deployment.read("builder", "notes.txt").as_deref(), Some("hello\n"));

    // The reconnect carries the last sequence number and replays what the client missed.
    let replayed = enterprise_until_idle(&mut events).await;
    let kinds: Vec<&str> = replayed.iter().map(|(_, kind)| kind.as_str()).collect();
    assert_eq!(kinds, ["status.running", "agent.message", "status.idle:end_turn"]);
    let last_parked = parked.last().unwrap().0;
    assert_eq!(
        *platform.connections.lock().unwrap(),
        [None, Some(last_parked.to_string())],
        "one reconnect, from the parked turn's last event"
    );
    let seqs: Vec<u64> = parked.iter().chain(&replayed).map(|(seq, _)| *seq).collect();
    assert_eq!(seqs, (0..seqs.len() as u64).collect::<Vec<_>>(), "every event once, in order");
}
