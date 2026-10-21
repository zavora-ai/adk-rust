//! One harness, every deployment (spec 3.1 §5 H9, §7.18, §8.5).
//!
//! The same `HarnessAgent` value runs behind `Launcher`, `ServerBuilder`,
//! `serve_agent_engine`'s app and a `ManagedAgentDef` on the managed runtime, with one
//! approvals inbox. On each surface a host `bash` is held, approved and resumed, and the
//! pass condition is the log the commands append to: one line per surface, in order.
//!
//! The live scenario runs a `HarnessDef` through the managed runtime, its model resolved by
//! `DefaultModelResolver` from the definition's model reference, and the same harness
//! through the Agent Engine entrypoint served on a loopback port.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use adk_cli::harness::{BearerTokenExtractor, OPERATOR_SCOPE};
use adk_core::principal::SubjectRef;
use adk_core::{
    ApprovalDecision, ApprovalPolicy, ApprovalRequest, ApprovalService, ApproverSelector, Event, InMemoryApprovalStore,
    InMemoryRunStore, InboxQuery, Llm, SingleAgentLoader, StoreApprovalService,
};
use adk_harness::packs::managed::{HarnessDef, HarnessResolver, ShellPolicy};
use adk_harness::{HarnessAgent, HarnessMode, InstructionsConfig};
use adk_managed::types::{ContentBlock, ManagedAgentDef, ModelRef, SessionEvent, StopReason, UserEvent};
use adk_managed::{
    DefaultManagedAgentRuntime, InMemoryManagedStateStore, ManagedAgentRuntime, ManagedOwner, ManagedStores,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use anyhow::{Context, Result, anyhow};
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::{Value, json};

use crate::{Checks, Subject};

pub const SCRIPTED: &[&str] = &["one_harness_every_surface"];

pub const LIVE: &[&str] = &["live_managed_and_agent_engine_roundtrip"];

const TOKEN: &str = "operator-token";
const USER: &str = "ana";
/// The harness name; `Launcher`, `ServerBuilder` and Agent Engine use it as the app name.
const NAME: &str = "harness";

/// Runs scripted scenario `name`, if it is one of these.
pub async fn run_scripted(name: &str, subject: Subject) -> Option<Result<Checks>> {
    match (name, subject) {
        ("one_harness_every_surface", Subject::Baseline) => {
            let mut checks = Checks::default();
            checks.check(
                "the 3.0 CodingAgent runs bash without a hold, so no surface has a held call to approve",
                false,
            );
            Some(Ok(checks))
        }
        ("one_harness_every_surface", Subject::Harness) => Some(one_harness_every_surface().await),
        _ => None,
    }
}

/// Runs live scenario `name` on `model`, if it is one of these.
pub async fn run_live(name: &str, model_name: &str, model: Arc<dyn Llm>) -> Option<Result<Checks>> {
    match name {
        "live_managed_and_agent_engine_roundtrip" => Some(live_roundtrip(model_name, model).await),
        _ => None,
    }
}

/// One inbox for every surface: operators decide, and the managed runtime's parked runs
/// wake when they do.
fn inbox(runs: Arc<InMemoryRunStore>) -> Arc<dyn ApprovalService> {
    let routing = ApprovalPolicy::new([ApproverSelector::group(OPERATOR_SCOPE)]).allow_self_approval();
    Arc::new(
        StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
            .with_router(routing)
            .with_run_store(runs),
    )
}

fn no_user_file() -> InstructionsConfig {
    InstructionsConfig { user_file: None, ..InstructionsConfig::default() }
}

fn log_command(surface: &str) -> Value {
    json!({ "command": format!("printf '{surface}\\n' >> log.txt") })
}

fn read_log(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("log.txt")).unwrap_or_default()
}

/// Serves `app` on a loopback port, with peer addresses, and returns its base URL.
async fn serve(app: axum::Router) -> Result<String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await
    });
    Ok(format!("http://{address}"))
}

/// The pending request of `session` in the inbox.
async fn pending(service: &Arc<dyn ApprovalService>, session: &str) -> Result<ApprovalRequest> {
    let mut query = InboxQuery::default();
    query.groups = vec![OPERATOR_SCOPE.to_string()];
    let page = service.inbox(query).await?;
    page.items
        .into_iter()
        .find(|item| item.scope.session_id == session)
        .ok_or_else(|| anyhow!("no pending approval for session {session}"))
}

async fn approve(service: &Arc<dyn ApprovalService>, request: &ApprovalRequest) -> Result<()> {
    let decision = ApprovalDecision::approve(SubjectRef::human(USER)).with_groups([OPERATOR_SCOPE]);
    service.decide(&request.id, request.version, decision).await?;
    Ok(())
}

// --- REST surfaces: Launcher and ServerBuilder ------------------------------------------

/// One `POST /api/run`: the turn's events.
async fn rest_run(base: &str, session: &str, text: Option<&str>) -> Result<Value> {
    let parts: Vec<Value> = text.map(|text| json!({ "text": text })).into_iter().collect();
    let body = json!({
        "appName": NAME,
        "userId": USER,
        "sessionId": session,
        "newMessage": { "role": "user", "parts": parts },
    });
    let response = reqwest::Client::new()
        .post(format!("{base}/api/run"))
        .bearer_auth(TOKEN)
        .json(&body)
        .send()
        .await?;
    let status = response.status();
    let events: Value = response.json().await?;
    anyhow::ensure!(status.is_success(), "POST /api/run: {status}: {events}");
    Ok(events)
}

/// Held over REST, decided through `/api/approvals`, resumed by an empty-message run.
async fn rest_roundtrip(checks: &mut Checks, surface: &str, base: &str, session: &str, dir: &Path, before: &str) -> Result<()> {
    let held = rest_run(base, session, Some("log the surface")).await?;
    checks.check(format!("{surface}: the call is held in the event stream"), held.to_string().contains("tool_confirmation"));
    checks.check(format!("{surface}: nothing ran while held"), read_log(dir) == before);

    let client = reqwest::Client::new();
    let inbox: Value = client.get(format!("{base}/api/approvals")).bearer_auth(TOKEN).send().await?.json().await?;
    // One request is pending at a time: each earlier surface's was decided.
    let item = inbox["items"].get(0).cloned().ok_or_else(|| anyhow!("{surface}: empty inbox: {inbox}"))?;
    let id = item["id"].as_str().context("approval id")?;
    let decide = json!({ "expectedVersion": item["version"], "action": "approve" });
    let status = client
        .post(format!("{base}/api/approvals/{id}/decide"))
        .bearer_auth(TOKEN)
        .json(&decide)
        .send()
        .await?
        .status();
    checks.check(format!("{surface}: approved over the approvals REST ({status})"), status.is_success());

    rest_run(base, session, None).await?;
    checks.check(format!("{surface}: the resumed call ran once"), read_log(dir) == format!("{before}{surface}\n"));
    Ok(())
}

// --- Agent Engine ----------------------------------------------------------------------

/// One `stream_query` dispatch: the newline-delimited events of the turn.
async fn stream_query(base: &str, session: &str, message: &str) -> Result<Vec<Event>> {
    let body = json!({
        "class_method": "stream_query",
        "input": { "user_id": USER, "session_id": session, "message": message },
    });
    let response = reqwest::Client::new().post(format!("{base}/api/stream_reasoning_engine")).json(&body).send().await?;
    let status = response.status();
    let text = response.text().await?;
    anyhow::ensure!(status.is_success(), "stream_query: {status}: {text}");
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<Event>(line).with_context(|| format!("not an event: {line}")))
        .collect()
}

fn held_in(events: &[Event]) -> bool {
    events.iter().any(|event| event.actions.tool_confirmation.is_some())
}

// --- Managed runtime -------------------------------------------------------------------

/// The events up to and including the next `status.idle`.
async fn until_idle(stream: &mut BoxStream<'static, SessionEvent>) -> Result<Vec<SessionEvent>> {
    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(120), stream.next())
            .await
            .context("no status.idle")?
            .context("the session stream ended")?;
        let idle = matches!(event, SessionEvent::StatusIdle { .. });
        events.push(event);
        if idle {
            return Ok(events);
        }
    }
}

fn requires_action(events: &[SessionEvent]) -> Vec<String> {
    match events.last() {
        Some(SessionEvent::StatusIdle { stop_reason: Some(StopReason::RequiresAction { event_ids }), .. }) => {
            event_ids.clone()
        }
        _ => Vec::new(),
    }
}

fn message(text: &str) -> UserEvent {
    UserEvent::Message { content: vec![ContentBlock::Text { text: text.into() }] }
}

// --- Scripted: one harness, four surfaces ----------------------------------------------

async fn one_harness_every_surface() -> Result<Checks> {
    let mut checks = Checks::default();
    let dir = tempfile::tempdir()?;
    let runs = Arc::new(InMemoryRunStore::new());
    let service = inbox(runs.clone());
    let mut script = Vec::new();
    for surface in ["launcher", "server", "engine", "managed"] {
        script.push(ScriptedResponse::function_call("bash", log_command(surface)));
        script.push(ScriptedResponse::text(format!("Logged {surface}.")));
    }
    let harness = HarnessAgent::builder(NAME)
        .model(Arc::new(ScriptedLlm::new("scripted", script)))
        .workspace(adk_devtools::Workspace::new(dir.path()).allow_bash(true))
        .project_instructions(no_user_file())
        .mode(HarnessMode::Ask)
        .approval_service(service.clone())
        .build()?;
    let sessions = Arc::new(InMemorySessionService::new());
    for session in ["launcher", "server"] {
        sessions
            .create(CreateRequest {
                app_name: NAME.into(),
                user_id: USER.into(),
                session_id: Some(session.into()),
                state: Default::default(),
            })
            .await?;
    }
    let extractor = || Arc::new(BearerTokenExtractor::new(TOKEN, USER));

    // 1. Launcher.
    // The example installed its own tracing subscriber.
    let app = adk_cli::Launcher::new(harness.agent())
        .with_telemetry(adk_cli::launcher::TelemetryConfig::None)
        .with_session_service(sessions.clone())
        .with_user_id(USER)
        .with_approval_service(service.clone())
        .with_request_context_extractor(extractor())
        .build_app()?;
    rest_roundtrip(&mut checks, "launcher", &serve(app).await?, "launcher", dir.path(), "").await?;

    // 2. ServerBuilder.
    let config = adk_server::ServerConfig::new(Arc::new(SingleAgentLoader::new(harness.agent())), sessions.clone())
        .with_request_context(extractor());
    let app = adk_server::ServerBuilder::new(config).with_approval_service(service.clone()).build();
    rest_roundtrip(&mut checks, "server", &serve(app).await?, "server", dir.path(), "launcher\n").await?;

    // 3. serve_agent_engine's app, driven through its class-method dispatch.
    let app = adk_server::agent_engine::build_agent_engine_app(
        harness.agent(),
        adk_server::agent_engine::AgentEngineOptions::new().with_session_service(sessions.clone()),
    )?;
    let base = serve(app).await?;
    let held = stream_query(&base, "engine", "log the surface").await?;
    checks.check("engine: the call is held in the dispatch stream", held_in(&held));
    approve(&service, &pending(&service, "engine").await?).await?;
    stream_query(&base, "engine", "").await?;
    checks.check("engine: the resumed call ran once", read_log(dir.path()) == "launcher\nserver\nengine\n");

    // 4. The managed runtime, for a definition named like the harness.
    let stores = ManagedStores::new(Arc::new(InMemoryManagedStateStore::new()), runs, Arc::new(InMemoryApprovalStore::new()))
        .with_approval_service(service.clone());
    let runtime = DefaultManagedAgentRuntime::new(Arc::new(adk_managed::DefaultModelResolver::new()), sessions.clone())
        .with_stores(stores)
        .with_durable_config(adk_managed::DurableConfig::default().with_poll_interval(Duration::from_millis(50)))
        .with_agent_resolver(Arc::new(HarnessResolver::new(dir.path().join("unused")).with_harness(&harness)));
    let agent = runtime.create(ManagedAgentDef::new(NAME, ModelRef::Shorthand("gemini-3.7-flash".into()))).await?;
    let session = runtime.start_session(&agent, &ManagedOwner::new("managed-app", USER)?, None).await?;
    let mut stream = runtime.stream_events(&session, None).await?;
    runtime.send_event(&session, message("log the surface")).await?;
    let parked = until_idle(&mut stream).await?;
    checks.check("managed: the turn parks on its durable run with requires_action", !requires_action(&parked).is_empty());
    approve(&service, &pending(&service, &session.0).await?).await?;
    let resumed = until_idle(&mut stream).await?;
    checks.check(
        "managed: the decision wakes the parked run and the call ran once",
        requires_action(&resumed).is_empty() && read_log(dir.path()) == "launcher\nserver\nengine\nmanaged\n",
    );
    Ok(checks)
}

// --- Live: the managed runtime and Agent Engine on a real model ------------------------

const LIVE_TASK: &str =
    "Create the file notes.txt containing exactly the word hello, using the write_file tool. Then say done.";

fn notes(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join("notes.txt")).ok()
}

async fn live_roundtrip(model_name: &str, model: Arc<dyn Llm>) -> Result<Checks> {
    let mut checks = Checks::default();
    // Each surface reports on its own, so one provider error does not hide the other.
    if let Err(error) = live_managed(&mut checks, model_name).await {
        checks.check(format!("managed: {error}"), false);
    }
    if let Err(error) = live_agent_engine(&mut checks, model).await {
        checks.check(format!("engine: {error}"), false);
    }
    for (name, ok) in &checks.0 {
        println!("    {} {name}", if *ok { "ok  " } else { "FAIL" });
    }
    Ok(checks)
}

/// The managed runtime builds the harness from a `HarnessDef`; `DefaultModelResolver`
/// resolves its model reference from the provider key in the environment.
async fn live_managed(checks: &mut Checks, model_name: &str) -> Result<()> {
    let workspaces = tempfile::tempdir()?;
    let runtime = DefaultManagedAgentRuntime::new(
        Arc::new(adk_managed::DefaultModelResolver::new()),
        Arc::new(InMemorySessionService::new()),
    )
    .with_agent_resolver(Arc::new(HarnessResolver::new(workspaces.path())));
    let def = HarnessDef::new("live-builder", ModelRef::Shorthand(model_name.into()))
        .with_mode(HarnessMode::Ask)
        .with_shell(ShellPolicy::none());
    let agent = runtime.create(def.into()).await?;
    let session = runtime.start_session(&agent, &ManagedOwner::new("live", USER)?, None).await?;
    let mut stream = runtime.stream_events(&session, None).await?;
    runtime.send_event(&session, message(LIVE_TASK)).await?;
    let mut turn = until_idle(&mut stream).await?;
    let dir = workspaces.path().join("live-builder");
    checks.check("managed: the write is held before it runs", !requires_action(&turn).is_empty() && notes(&dir).is_none());
    // The model may issue the write again after its result; each one is held and allowed.
    for _ in 0..3 {
        let waiting = requires_action(&turn);
        if waiting.is_empty() {
            break;
        }
        for id in waiting {
            let allow = UserEvent::ToolConfirmation {
                tool_use_id: id,
                result: adk_managed::types::ConfirmationResult::Allow,
                deny_message: None,
            };
            runtime.send_event(&session, allow).await?;
        }
        turn = until_idle(&mut stream).await?;
    }
    for event in &turn {
        if let SessionEvent::Error { code, message, .. } = event {
            checks.check(format!("managed: error event {code}: {message}"), false);
        }
    }
    checks.check(
        format!("managed: after approval notes.txt holds hello ({:?})", notes(&dir)),
        notes(&dir).is_some_and(|text| text.trim() == "hello"),
    );
    Ok(())
}

/// The same model behind `serve_agent_engine`'s app on a loopback port.
async fn live_agent_engine(checks: &mut Checks, model: Arc<dyn Llm>) -> Result<()> {
    let dir = tempfile::tempdir()?;
    let service = inbox(Arc::new(InMemoryRunStore::new()));
    let harness = HarnessAgent::builder(NAME)
        .model(model)
        .workspace(adk_devtools::Workspace::new(dir.path()))
        .project_instructions(no_user_file())
        .mode(HarnessMode::Ask)
        .approval_service(service.clone())
        .build()?;
    let app = adk_server::agent_engine::build_agent_engine_app(
        harness.agent(),
        adk_server::agent_engine::AgentEngineOptions::new(),
    )?;
    let base = serve(app).await?;
    let mut events = stream_query(&base, "live", LIVE_TASK).await?;
    checks.check("engine: the write is held before it runs", held_in(&events) && notes(dir.path()).is_none());
    for _ in 0..3 {
        if !held_in(&events) {
            break;
        }
        approve(&service, &pending(&service, "live").await?).await?;
        events = stream_query(&base, "live", "").await?;
    }
    checks.check(
        format!("engine: after approval notes.txt holds hello ({:?})", notes(dir.path())),
        notes(dir.path()).is_some_and(|text| text.trim() == "hello"),
    );
    Ok(())
}
