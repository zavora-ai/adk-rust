//! Session scoping and integrity, end to end.
//!
//! Every scenario runs over each configured session backend (in-memory, encrypted, SQLite,
//! PostgreSQL, Redis) and drives the real stack: `LlmAgentBuilder`, `Runner`, the session
//! service, `OpenAIClient`, and the `adk-server` REST and A2A endpoints on 127.0.0.1. The
//! deterministic scenarios point the client at a scripted OpenAI-compatible server; faults the
//! backends cannot be made to produce on demand (a transient `get` failure, two first requests
//! racing to create a session) come from a session-service wrapper. `live_recovery` runs against
//! OpenAI when `OPENAI_API_KEY` is set.
//!
//! | Variable | Used by | Example |
//! |----------|---------|---------|
//! | `POSTGRES_URL` | the `postgres` backend | `postgres://adk@127.0.0.1:5432/sessions` |
//! | `REDIS_URL` | the `redis` backend | `redis://127.0.0.1:6379` |
//! | `OPENAI_API_KEY` | `live_recovery` | |
//!
//! ```bash
//! cargo run --manifest-path examples/session_integrity/Cargo.toml
//! cargo run --manifest-path examples/session_integrity/Cargo.toml -- \
//!     --scenario rewind_scoped_to_user --backend memory,sqlite
//! ```

mod scripted;

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use adk_core::{
    AdkError, Agent, Content, ErrorCategory, ErrorComponent, Event, FunctionResponseData, Llm,
    Part, SingleAgentLoader, Tool, ToolContext,
};
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use adk_runner::Runner;
use adk_server::{ServerConfig, create_app_with_a2a};
use adk_session::{
    AppendEventRequest, CreateRequest, DeleteRequest, EncryptedSession, EncryptionKey, GetRequest,
    InMemorySessionService, ListRequest, PostgresSessionService, RedisSessionConfig,
    RedisSessionService, Session, SessionService, SqliteSessionService,
};
use adk_tool::FunctionTool;
use async_trait::async_trait;
use chrono::{DateTime, SubsecRound, TimeZone, Utc};
use futures::StreamExt;
use scripted::{Reply, ScriptedServer, message_text, messages};
use serde_json::{Value, json};

/// The app name and the agent name: the server loads the agent by app name, and the legacy A2A
/// endpoint runs the root agent under its own name.
const APP: &str = "integrity_assistant";
const AGENT_NAME: &str = APP;
const LIVE_MODEL: &str = "gpt-5.6-terra";
const BLOB_TEXT: &str = "PK\u{0}\u{3}binary";
const BLOB_STATE: &str = "a\u{0}b";

// ---------------------------------------------------------------------------
// Verdicts
// ---------------------------------------------------------------------------

enum Verdict {
    Pass(String),
    Fail(String),
    Skip(String),
}

/// Each check is `(holds, what it asserts or observed)`; the evidence marks the ones that fail.
fn verdict(checks: Vec<(bool, String)>) -> Verdict {
    let all_hold = checks.iter().all(|(holds, _)| *holds);
    let evidence = checks
        .into_iter()
        .map(|(holds, message)| if holds { message } else { format!("[FAILED] {message}") })
        .collect::<Vec<_>>()
        .join("\n      ");
    if all_hold { Verdict::Pass(evidence) } else { Verdict::Fail(evidence) }
}

fn describe(error: &AdkError) -> String {
    format!("code={} category={:?}", error.code, error.category)
}

// ---------------------------------------------------------------------------
// Backends
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Backend {
    Memory,
    Encrypted,
    Sqlite,
    Postgres,
    Redis,
}

const BACKENDS: [Backend; 5] =
    [Backend::Memory, Backend::Encrypted, Backend::Sqlite, Backend::Postgres, Backend::Redis];

impl Backend {
    fn name(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Encrypted => "encrypted",
            Self::Sqlite => "sqlite",
            Self::Postgres => "postgres",
            Self::Redis => "redis",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        BACKENDS.into_iter().find(|backend| backend.name() == name)
    }
}

/// Opens a fresh service for `backend`, or `None` when its server is not configured.
async fn open(
    backend: Backend,
    dir: &tempfile::TempDir,
) -> anyhow::Result<Option<Arc<dyn SessionService>>> {
    let service: Arc<dyn SessionService> = match backend {
        Backend::Memory => Arc::new(InMemorySessionService::new()),
        Backend::Encrypted => Arc::new(EncryptedSession::new(
            InMemorySessionService::new(),
            EncryptionKey::generate(),
            vec![],
        )),
        Backend::Sqlite => {
            let path = dir.path().join(format!("{}.db", uuid::Uuid::new_v4().simple()));
            let service =
                SqliteSessionService::new(&format!("sqlite://{}?mode=rwc", path.display())).await?;
            service.migrate().await?;
            Arc::new(service)
        }
        Backend::Postgres => {
            let Ok(url) = std::env::var("POSTGRES_URL") else { return Ok(None) };
            let service = PostgresSessionService::new(&url).await?;
            service.migrate().await?;
            Arc::new(service)
        }
        Backend::Redis => {
            let Ok(url) = std::env::var("REDIS_URL") else { return Ok(None) };
            Arc::new(
                RedisSessionService::new(RedisSessionConfig {
                    url,
                    ttl: None,
                    cluster_nodes: None,
                })
                .await?,
            )
        }
    };
    Ok(Some(service))
}

/// A session service that fails the next `get` with a transient error when armed, and can
/// hold every `create` until a number of them are waiting, so first requests race.
struct Faults {
    inner: Arc<dyn SessionService>,
    fail_next_get: AtomicBool,
    create_gate: std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>,
}

impl Faults {
    fn wrap(inner: Arc<dyn SessionService>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            fail_next_get: AtomicBool::new(false),
            create_gate: std::sync::Mutex::new(None),
        })
    }

    fn fail_next_get(&self) {
        self.fail_next_get.store(true, Ordering::SeqCst);
    }

    fn gate_creates(&self, parties: Option<usize>) {
        *self.create_gate.lock().unwrap() =
            parties.map(|parties| Arc::new(tokio::sync::Barrier::new(parties)));
    }
}

#[async_trait]
impl SessionService for Faults {
    async fn create(&self, req: CreateRequest) -> adk_core::Result<Box<dyn Session>> {
        let gate = self.create_gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            gate.wait().await;
        }
        self.inner.create(req).await
    }

    async fn get(&self, req: GetRequest) -> adk_core::Result<Box<dyn Session>> {
        if self.fail_next_get.swap(false, Ordering::SeqCst) {
            return Err(AdkError::new(
                ErrorComponent::Session,
                ErrorCategory::Unavailable,
                "session.backend.timeout",
                "the session backend timed out",
            ));
        }
        self.inner.get(req).await
    }

    async fn list(&self, req: ListRequest) -> adk_core::Result<Vec<Box<dyn Session>>> {
        self.inner.list(req).await
    }

    async fn delete(&self, req: DeleteRequest) -> adk_core::Result<()> {
        self.inner.delete(req).await
    }

    async fn append_event(&self, session_id: &str, event: Event) -> adk_core::Result<()> {
        self.inner.append_event(session_id, event).await
    }

    async fn append_event_for_identity(&self, req: AppendEventRequest) -> adk_core::Result<()> {
        self.inner.append_event_for_identity(req).await
    }

    async fn delete_all_sessions(&self, app_name: &str, user_id: &str) -> adk_core::Result<()> {
        self.inner.delete_all_sessions(app_name, user_id).await
    }
}

// ---------------------------------------------------------------------------
// Session helpers
// ---------------------------------------------------------------------------

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

fn identity(app: &str, user: &str, session: &str) -> AdkIdentity {
    AdkIdentity::new(
        AppName::try_from(app).expect("valid app name"),
        UserId::try_from(user).expect("valid user id"),
        SessionId::try_from(session).expect("valid session id"),
    )
}

fn create_request(app: &str, user: &str, session: &str, state: Value) -> CreateRequest {
    let state = match state {
        Value::Object(map) => map.into_iter().collect(),
        _ => HashMap::new(),
    };
    CreateRequest {
        app_name: app.to_string(),
        user_id: user.to_string(),
        session_id: Some(session.to_string()),
        state,
    }
}

async fn fetch(
    sessions: &dyn SessionService,
    app: &str,
    user: &str,
    session: &str,
) -> adk_core::Result<Box<dyn Session>> {
    sessions
        .get(GetRequest {
            app_name: app.to_string(),
            user_id: user.to_string(),
            session_id: session.to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
}

/// `(event count, state)` of a stored session, or the read error.
async fn snapshot(
    sessions: &dyn SessionService,
    app: &str,
    user: &str,
    session: &str,
) -> Result<(usize, HashMap<String, Value>), String> {
    fetch(sessions, app, user, session)
        .await
        .map(|stored| (stored.events().len(), stored.state().all()))
        .map_err(|error| describe(&error))
}

async fn append(
    sessions: &dyn SessionService,
    app: &str,
    user: &str,
    session: &str,
    event: Event,
) -> adk_core::Result<()> {
    sessions
        .append_event_for_identity(AppendEventRequest {
            identity: identity(app, user, session),
            event,
        })
        .await
}

fn user_text_event(text: &str) -> Event {
    let mut event = Event::new("seed");
    event.author = "user".to_string();
    event.set_content(Content::new("user").with_text(text));
    event
}

/// Rewinds `steps` events of the session `(APP, user, session)`.
async fn rewind_steps(
    sessions: &dyn SessionService,
    user: &str,
    session: &str,
    steps: usize,
) -> adk_core::Result<Box<dyn Session>> {
    sessions.rewind_steps(&identity(APP, user, session), steps).await
}

fn rewind_unsupported(error: &AdkError) -> bool {
    error.category == ErrorCategory::Unsupported || error.message.contains("not supported")
}

// ---------------------------------------------------------------------------
// Agent, model, runner, server
// ---------------------------------------------------------------------------

/// An order lookup that writes one state key per tier (app, user, session).
fn lookup_tool() -> Arc<dyn Tool> {
    Arc::new(FunctionTool::new(
        "lookup_fact",
        "Looks up the stored fact for an order. Arguments: order_id (string).",
        |ctx: Arc<dyn ToolContext>, _args: Value| async move {
            let mut actions = ctx.actions();
            actions.state_delta.insert("app:lookups".to_string(), json!(1));
            actions.state_delta.insert("user:last_lookup".to_string(), json!("FACT-7"));
            actions.state_delta.insert("last_order".to_string(), json!("7"));
            ctx.set_actions(actions);
            Ok(json!({"fact": "FACT-7", "status": "shipped"}))
        },
    ))
}

/// Reads a binary file as text: the result and a state value contain U+0000.
fn blob_tool() -> Arc<dyn Tool> {
    Arc::new(FunctionTool::new(
        "read_blob",
        "Reads the uploaded file as text. No arguments.",
        |ctx: Arc<dyn ToolContext>, _args: Value| async move {
            let mut actions = ctx.actions();
            actions.state_delta.insert("blob".to_string(), json!(BLOB_STATE));
            ctx.set_actions(actions);
            Ok(json!({"text": BLOB_TEXT}))
        },
    ))
}

/// Calls `lookup_fact` for "look up", `read_blob` for "read the blob", answers "noted" after
/// a tool result, and "ok" otherwise.
fn agent_script() -> scripted::Script {
    Box::new(|request: &Value| {
        let messages = messages(request);
        let last = messages.last().cloned().unwrap_or(Value::Null);
        let text = message_text(&last).to_lowercase();
        match last.get("role").and_then(Value::as_str) {
            Some("tool") => Reply::Text("noted".to_string()),
            Some("user") if text.contains("look up") => Reply::Call(
                format!("call_{}", messages.len()),
                "lookup_fact".to_string(),
                r#"{"order_id":"7"}"#.to_string(),
            ),
            Some("user") if text.contains("read the blob") => Reply::Call(
                format!("call_{}", messages.len()),
                "read_blob".to_string(),
                "{}".to_string(),
            ),
            _ => Reply::Text("ok".to_string()),
        }
    })
}

fn scripted_model(server: &ScriptedServer) -> anyhow::Result<Arc<dyn Llm>> {
    Ok(Arc::new(OpenAIClient::compatible("scripted-key", &server.base_url, "scripted-model")?))
}

fn build_agent(model: Arc<dyn Llm>) -> anyhow::Result<Arc<dyn Agent>> {
    Ok(Arc::new(
        LlmAgentBuilder::new(AGENT_NAME)
            .description("Answers questions, looks up orders, and reads files.")
            .instruction(
                "You are a concise assistant. Use lookup_fact to look up an order and read_blob \
                 to read the uploaded file.",
            )
            .model(model)
            .tool(lookup_tool())
            .tool(blob_tool())
            .build()?,
    ))
}

fn runner_for(agent: Arc<dyn Agent>, sessions: Arc<dyn SessionService>) -> anyhow::Result<Runner> {
    Ok(Runner::builder().app_name(APP).agent(agent).session_service(sessions).build()?)
}

/// Runs one user turn through the runner and returns the final text, or the error that ended it.
async fn turn(runner: &Runner, user: &str, session: &str, text: &str) -> Result<String, String> {
    let mut stream = runner
        .run_str(user, session, Content::new("user").with_text(text))
        .await
        .map_err(|error| describe(&error))?;
    let mut reply = String::new();
    while let Some(item) = stream.next().await {
        let event = item.map_err(|error| describe(&error))?;
        if event.llm_response.partial {
            continue;
        }
        if let Some(content) = &event.llm_response.content {
            let text: String = content.parts.iter().filter_map(Part::text).collect();
            if !text.is_empty() {
                reply = text;
            }
        }
    }
    Ok(reply)
}

/// An `adk-server` app (REST and legacy A2A) listening on 127.0.0.1.
struct HttpServer {
    base: String,
    client: reqwest::Client,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl HttpServer {
    async fn start(
        agent: Arc<dyn Agent>,
        sessions: Arc<dyn SessionService>,
    ) -> anyhow::Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let config = ServerConfig::new(Arc::new(SingleAgentLoader::new(agent)), sessions);
        let app = create_app_with_a2a(config, Some(&base));
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self { base, client: reqwest::Client::new(), task })
    }

    async fn post(&self, path: &str, body: Value) -> anyhow::Result<(u16, String)> {
        let response = self.client.post(format!("{}{path}", self.base)).json(&body).send().await?;
        let status = response.status().as_u16();
        Ok((status, response.text().await?))
    }

    async fn get(&self, path: &str) -> anyhow::Result<(u16, String)> {
        let response = self.client.get(format!("{}{path}", self.base)).send().await?;
        let status = response.status().as_u16();
        Ok((status, response.text().await?))
    }

    /// `POST /api/run` (or `/api/run_sse` when `sse`) for `(APP, user, session)`.
    async fn run(
        &self,
        sse: bool,
        user: &str,
        session: &str,
        text: &str,
    ) -> anyhow::Result<(u16, String)> {
        let path = if sse { "/api/run_sse" } else { "/api/run" };
        self.post(
            path,
            json!({
                "appName": APP,
                "userId": user,
                "sessionId": session,
                "newMessage": {"role": "user", "parts": [{"text": text}]},
            }),
        )
        .await
    }

    /// A legacy A2A `message/send` in `context_id`; returns the JSON-RPC response.
    async fn a2a_send(&self, context_id: &str, text: &str) -> anyhow::Result<Value> {
        let (_, body) = self
            .post(
                "/a2a",
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "message/send",
                    "params": {"message": {
                        "role": "user",
                        "parts": [{"kind": "text", "text": text}],
                        "messageId": unique("msg"),
                        "contextId": context_id,
                    }},
                }),
            )
            .await?;
        Ok(serde_json::from_str(&body).unwrap_or(Value::Null))
    }
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

struct Ctx<'a> {
    backend: Backend,
    dir: &'a tempfile::TempDir,
}

macro_rules! open_or_skip {
    ($ctx:expr) => {
        match open($ctx.backend, $ctx.dir).await? {
            Some(sessions) => sessions,
            None => {
                return Ok(Verdict::Skip(format!(
                    "{} is not configured (set its URL variable)",
                    $ctx.backend.name()
                )));
            }
        }
    };
}

/// Two users own the same session ID; each rewind changes only the caller's session.
async fn rewind_scoped_to_user(ctx: &Ctx<'_>) -> anyhow::Result<Verdict> {
    let sessions = open_or_skip!(ctx);
    let model_server = ScriptedServer::start(agent_script()).await?;
    let runner = runner_for(build_agent(scripted_model(&model_server)?)?, sessions.clone())?;
    let shared = unique("shared");

    for user in ["alice", "bob"] {
        sessions.create(create_request(APP, user, &shared, json!({"owner": user}))).await?;
        for text in ["First turn.", "Second turn."] {
            turn(&runner, user, &shared, text).await.map_err(anyhow::Error::msg)?;
        }
    }
    let alice_before = snapshot(&*sessions, APP, "alice", &shared).await;

    let bob_rewound = rewind_steps(&*sessions, "bob", &shared, 2).await;
    if let Err(error) = &bob_rewound
        && rewind_unsupported(error)
    {
        return Ok(Verdict::Skip(format!(
            "the backend does not implement rewind ({})",
            describe(error)
        )));
    }
    let alice_after_bob = snapshot(&*sessions, APP, "alice", &shared).await;
    let bob_after_bob = snapshot(&*sessions, APP, "bob", &shared).await;
    let alice_rewound = rewind_steps(&*sessions, "alice", &shared, 2).await;
    let alice_after = snapshot(&*sessions, APP, "alice", &shared).await;
    let bob_after = snapshot(&*sessions, APP, "bob", &shared).await;

    let count = |snapshot: &Result<(usize, HashMap<String, Value>), String>| {
        snapshot.as_ref().map(|(events, _)| *events).unwrap_or(usize::MAX)
    };
    let owner = |snapshot: &Result<(usize, HashMap<String, Value>), String>| {
        snapshot.as_ref().ok().and_then(|(_, state)| state.get("owner").cloned())
    };
    Ok(verdict(vec![
        (
            count(&alice_before) == 4,
            format!("alice and bob each hold 4 events in session {shared}"),
        ),
        (
            bob_rewound.as_ref().is_ok_and(|session| session.user_id() == "bob"),
            format!(
                "rewind_steps(bob, 2) returns bob's session: {:?}",
                bob_rewound.as_ref().map(|s| s.user_id().to_string()).map_err(describe)
            ),
        ),
        (
            count(&alice_after_bob) == 4 && owner(&alice_after_bob) == Some(json!("alice")),
            format!("alice's session is untouched by bob's rewind: {alice_after_bob:?}"),
        ),
        (
            count(&bob_after_bob) == 2,
            format!("bob's session lost its last turn: {bob_after_bob:?}"),
        ),
        (
            alice_rewound.is_ok() && count(&alice_after) == 2,
            format!("rewind_steps(alice, 2) leaves alice 2 events: {alice_after:?}"),
        ),
        (
            count(&bob_after) == 2,
            format!("bob's session is untouched by alice's rewind: {bob_after:?}"),
        ),
    ]))
}

/// State set at `create` survives rewinding part of the history and all of it.
async fn rewind_keeps_initial_state(ctx: &Ctx<'_>) -> anyhow::Result<Verdict> {
    let sessions = open_or_skip!(ctx);
    let model_server = ScriptedServer::start(agent_script()).await?;
    let runner = runner_for(build_agent(scripted_model(&model_server)?)?, sessions.clone())?;
    let user = "carol";
    let session = unique("tiered");
    sessions.create(create_request(APP, user, &session, json!({"tier": "gold"}))).await?;
    turn(&runner, user, &session, "Please look up order 7.").await.map_err(anyhow::Error::msg)?;
    turn(&runner, user, &session, "Thanks.").await.map_err(anyhow::Error::msg)?;
    let before = snapshot(&*sessions, APP, user, &session).await;

    let partial = rewind_steps(&*sessions, user, &session, 2).await;
    if let Err(error) = &partial
        && rewind_unsupported(error)
    {
        return Ok(Verdict::Skip(format!(
            "the backend does not implement rewind ({})",
            describe(error)
        )));
    }
    let partial_state = partial.as_ref().map(|s| s.state().all()).map_err(describe);
    let partial_reread = snapshot(&*sessions, APP, user, &session).await;
    let all = rewind_steps(&*sessions, user, &session, 4).await;
    let all_state = all.as_ref().map(|s| s.state().all()).map_err(describe);
    let all_reread = snapshot(&*sessions, APP, user, &session).await;

    let key =
        |state: Option<&HashMap<String, Value>>, key: &str| state.and_then(|s| s.get(key).cloned());
    let gold = Some(json!("gold"));
    Ok(verdict(vec![
        (
            before.as_ref().is_ok_and(|(events, _)| *events == 6),
            format!("two turns stored 6 events: {:?}", before.as_ref().map(|(n, _)| *n)),
        ),
        (
            key(partial_state.as_ref().ok(), "tier") == gold
                && key(partial_state.as_ref().ok(), "last_order") == Some(json!("7")),
            format!("rewinding the last turn keeps tier and last_order: {partial_state:?}"),
        ),
        (
            partial_reread
                .as_ref()
                .is_ok_and(|(events, state)| *events == 4 && state.get("tier") == gold.as_ref()),
            format!("re-read after the partial rewind: {partial_reread:?}"),
        ),
        (
            key(all_state.as_ref().ok(), "tier") == gold
                && key(all_state.as_ref().ok(), "last_order").is_none(),
            format!("rewinding every event keeps tier and drops last_order: {all_state:?}"),
        ),
        (
            all_reread
                .as_ref()
                .is_ok_and(|(events, state)| *events == 0 && state.get("tier") == gold.as_ref()),
            format!("re-read after rewinding every event: {all_reread:?}"),
        ),
    ]))
}

/// A second `create` for an existing identity fails with a typed conflict and changes nothing.
async fn duplicate_create_conflicts(ctx: &Ctx<'_>) -> anyhow::Result<Verdict> {
    let sessions = open_or_skip!(ctx);
    let model_server = ScriptedServer::start(agent_script()).await?;
    let agent = build_agent(scripted_model(&model_server)?)?;
    let runner = runner_for(agent.clone(), sessions.clone())?;
    let http = HttpServer::start(agent, sessions.clone()).await?;
    let user = "dave";
    let session = unique("dup");
    let flag = format!("app:flag_{}", uuid::Uuid::new_v4().simple());

    let mut seeded = json!({"plan": "gold"});
    seeded[&flag] = json!("on");
    sessions.create(create_request(APP, user, &session, seeded)).await?;
    turn(&runner, user, &session, "Hello.").await.map_err(anyhow::Error::msg)?;
    let before = snapshot(&*sessions, APP, user, &session).await;

    let mut replacement = json!({"plan": "free"});
    replacement[&flag] = json!("off");
    let duplicate = sessions.create(create_request(APP, user, &session, replacement)).await;
    let after = snapshot(&*sessions, APP, user, &session).await;
    let (status, _) = http
        .post(
            &format!("/api/apps/{APP}/users/{user}/sessions/{session}"),
            json!({"state": {"plan": "free"}}),
        )
        .await?;
    let after_http = snapshot(&*sessions, APP, user, &session).await;

    let unchanged = |snapshot: &Result<(usize, HashMap<String, Value>), String>| {
        snapshot.as_ref().is_ok_and(|(events, state)| {
            Some(*events) == before.as_ref().ok().map(|(n, _)| *n)
                && state.get("plan") == Some(&json!("gold"))
                && state.get(&flag) == Some(&json!("on"))
        })
    };
    Ok(verdict(vec![
        (
            duplicate
                .as_ref()
                .err()
                .is_some_and(|e| e.is_conflict() && e.code == "session.already_exists"),
            format!(
                "the second create fails with a conflict: {}",
                match &duplicate {
                    Ok(_) => "Ok (the session was replaced)".to_string(),
                    Err(error) => describe(error),
                }
            ),
        ),
        (unchanged(&after), format!("events, state and the app tier are unchanged: {after:?}")),
        (
            status == 409,
            format!("POST /api/apps/{{app}}/users/{{user}}/sessions/{{existing}} returns {status}"),
        ),
        (unchanged(&after_http), format!("still unchanged after the HTTP create: {after_http:?}")),
    ]))
}

/// One transient `get` failure on `/api/run`, `/api/run_sse`, and A2A `message/send` fails the
/// request and leaves the stored session as it was; the next request continues it.
async fn transient_get_keeps_session(ctx: &Ctx<'_>) -> anyhow::Result<Verdict> {
    let sessions = open_or_skip!(ctx);
    let faults = Faults::wrap(sessions.clone());
    let model_server = ScriptedServer::start(agent_script()).await?;
    let agent = build_agent(scripted_model(&model_server)?)?;
    let http = HttpServer::start(agent, faults.clone()).await?;
    let mut checks = Vec::new();

    for endpoint in ["/api/run", "/api/run_sse", "a2a message/send"] {
        let a2a = endpoint.starts_with("a2a");
        let session = unique("transient");
        let (app, user) = if a2a {
            (AGENT_NAME.to_string(), format!("A2A_USER_{session}"))
        } else {
            (APP.to_string(), "erin".to_string())
        };
        sessions.create(create_request(&app, &user, &session, json!({"plan": "gold"}))).await?;
        for n in 0..3 {
            append(
                &*sessions,
                &app,
                &user,
                &session,
                user_text_event(&format!("earlier turn {n}")),
            )
            .await?;
        }

        faults.fail_next_get();
        let failed = if a2a {
            let response = http.a2a_send(&session, "Continue.").await?;
            (
                response.get("error").is_some(),
                format!("JSON-RPC error={}", response.get("error").is_some()),
            )
        } else {
            let (status, _) =
                http.run(endpoint == "/api/run_sse", &user, &session, "Continue.").await?;
            (status == 503, format!("status {status}"))
        };
        let after_failure = snapshot(&*sessions, &app, &user, &session).await;
        let retried = if a2a {
            http.a2a_send(&session, "Continue.").await?.get("result").is_some()
        } else {
            http.run(endpoint == "/api/run_sse", &user, &session, "Continue.").await?.0 == 200
        };
        let after_retry = snapshot(&*sessions, &app, &user, &session).await;

        checks.push((
            failed.0,
            format!("{endpoint} with a failing get reports the failure: {}", failed.1),
        ));
        checks.push((
            after_failure.as_ref().is_ok_and(|(events, state)| {
                *events == 3 && state.get("plan") == Some(&json!("gold"))
            }),
            format!("{endpoint}: the session keeps its 3 events and plan=gold: {after_failure:?}"),
        ));
        checks.push((
            retried
                && after_retry.as_ref().is_ok_and(|(events, state)| {
                    *events >= 5 && state.get("plan") == Some(&json!("gold"))
                }),
            format!(
                "{endpoint}: the retry continues the session: {:?}",
                after_retry.as_ref().map(|(n, _)| *n)
            ),
        ));
    }
    Ok(verdict(checks))
}

/// Two first requests for a new session race to create it; both run and both turns are stored.
async fn concurrent_first_requests(ctx: &Ctx<'_>) -> anyhow::Result<Verdict> {
    let sessions = open_or_skip!(ctx);
    let faults = Faults::wrap(sessions.clone());
    let model_server = ScriptedServer::start(agent_script()).await?;
    let agent = build_agent(scripted_model(&model_server)?)?;
    let http = HttpServer::start(agent, faults.clone()).await?;
    let user = "frank";
    let session = unique("race");

    faults.gate_creates(Some(2));
    let (first, second) = tokio::join!(
        http.run(false, user, &session, "First request."),
        http.run(false, user, &session, "Second request."),
    );
    faults.gate_creates(None);
    let (first, second) = (first?, second?);
    let stored = fetch(&*sessions, APP, user, &session).await;
    let user_turns = stored
        .as_ref()
        .map(|s| s.events().all().iter().filter(|event| event.author == "user").count())
        .map_err(describe);

    Ok(verdict(vec![
        (
            first.0 == 200 && second.0 == 200,
            format!("both POST /api/run return 200: {} and {}", first.0, second.0),
        ),
        (user_turns == Ok(2), format!("both user turns are stored: {user_turns:?}")),
    ]))
}

/// Appending to a missing session is a typed `session.not_found` on both append paths, and
/// the HTTP lookups answer 404.
async fn unknown_session_not_found(ctx: &Ctx<'_>) -> anyhow::Result<Verdict> {
    let sessions = open_or_skip!(ctx);
    let model_server = ScriptedServer::start(agent_script()).await?;
    let http =
        HttpServer::start(build_agent(scripted_model(&model_server)?)?, sessions.clone()).await?;
    let user = "gina";
    let missing = unique("missing");

    let by_identity = append(&*sessions, APP, user, &missing, user_text_event("hi")).await;
    let typed = |result: &adk_core::Result<()>| {
        result.as_ref().err().is_some_and(|e| e.is_not_found() && e.code == "session.not_found")
    };
    let shown = |result: &adk_core::Result<()>| match result {
        Ok(()) => "Ok".to_string(),
        Err(error) => describe(error),
    };
    let mut checks = vec![(
        typed(&by_identity),
        format!("append_event_for_identity to a missing session: {}", shown(&by_identity)),
    )];
    if ctx.backend != Backend::Encrypted {
        let by_id = sessions.append_event(&missing, user_text_event("hi")).await;
        checks
            .push((typed(&by_id), format!("append_event to a missing session: {}", shown(&by_id))));
    }
    let (run_status, _) = http
        .post(&format!("/api/run/{APP}/{user}/{missing}"), json!({"new_message": "hi"}))
        .await?;
    let (get_status, _) =
        http.get(&format!("/api/apps/{APP}/users/{user}/sessions/{missing}")).await?;
    checks.push((
        run_status == 404,
        format!("POST /api/run/{{app}}/{{user}}/{{missing}} returns {run_status}"),
    ));
    checks.push((
        get_status == 404,
        format!("GET /api/apps/{{app}}/users/{{user}}/sessions/{{missing}} returns {get_status}"),
    ));
    Ok(verdict(checks))
}

fn function_call_event(n: usize, at: DateTime<Utc>) -> Event {
    let mut event = Event::new("inv-order");
    event.author = AGENT_NAME.to_string();
    event.timestamp = at;
    event.set_content(Content {
        role: "model".into(),
        parts: vec![Part::FunctionCall {
            name: "lookup_fact".into(),
            args: json!({"order_id": n.to_string()}),
            id: Some(format!("call-{n}")),
            thought_signature: None,
        }],
    });
    event
}

fn function_response_event(n: usize, at: DateTime<Utc>) -> Event {
    let mut event = Event::new("inv-order");
    event.author = AGENT_NAME.to_string();
    event.timestamp = at;
    event.set_content(Content {
        role: "function".into(),
        parts: vec![Part::FunctionResponse {
            function_response: FunctionResponseData::new("lookup_fact", json!({"fact": n})),
            id: Some(format!("call-{n}")),
            annotations: None,
        }],
    });
    event
}

fn is_call(event: &Event) -> bool {
    event.llm_response.content.as_ref().is_some_and(|content| {
        content.parts.iter().any(|part| matches!(part, Part::FunctionCall { .. }))
    })
}

/// Events stored in the same millisecond, or at the same instant, read back in append order.
async fn same_millisecond_order(ctx: &Ctx<'_>) -> anyhow::Result<Verdict> {
    let sessions = open_or_skip!(ctx);
    let user = "hank";

    let trials = 40;
    let mut response_first = 0;
    for n in 0..trials {
        let session = unique("pair");
        sessions.create(create_request(APP, user, &session, json!({}))).await?;
        let base = Utc.timestamp_millis_opt(1_800_000_000_000 + n as i64).unwrap();
        append(
            &*sessions,
            APP,
            user,
            &session,
            function_call_event(n, base + chrono::Duration::microseconds(100)),
        )
        .await?;
        append(
            &*sessions,
            APP,
            user,
            &session,
            function_response_event(n, base + chrono::Duration::microseconds(400)),
        )
        .await?;
        let stored = fetch(&*sessions, APP, user, &session).await?;
        if !stored.events().at(0).is_some_and(is_call) {
            response_first += 1;
        }
    }

    let session = unique("ties");
    sessions.create(create_request(APP, user, &session, json!({}))).await?;
    let instant = Utc::now().trunc_subsecs(6);
    let mut appended = Vec::new();
    for n in 0..12 {
        let mut event = user_text_event(&format!("tied event {n}"));
        event.timestamp = instant;
        appended.push(event.id.clone());
        append(&*sessions, APP, user, &session, event).await?;
    }
    let read: Vec<String> = fetch(&*sessions, APP, user, &session)
        .await?
        .events()
        .all()
        .into_iter()
        .map(|e| e.id)
        .collect();

    Ok(verdict(vec![
        (
            response_first == 0,
            format!(
                "{response_first}/{trials} call/response pairs 300 µs apart in one millisecond read back response-first"
            ),
        ),
        (read == appended, "12 events with one timestamp read back in append order".to_string()),
    ]))
}

fn contains_string(value: &Value, wanted: &str) -> bool {
    match value {
        Value::String(text) => text == wanted,
        Value::Array(items) => items.iter().any(|item| contains_string(item, wanted)),
        Value::Object(map) => map.values().any(|item| contains_string(item, wanted)),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

/// A tool result and a state value containing U+0000 are stored and read back unchanged.
async fn nul_round_trip(ctx: &Ctx<'_>) -> anyhow::Result<Verdict> {
    let sessions = open_or_skip!(ctx);
    let model_server = ScriptedServer::start(agent_script()).await?;
    let runner = runner_for(build_agent(scripted_model(&model_server)?)?, sessions.clone())?;
    let user = "ivy";
    let session = unique("nul");
    sessions.create(create_request(APP, user, &session, json!({}))).await?;

    let turned = turn(&runner, user, &session, "Please read the blob.").await;
    let text_append =
        append(&*sessions, APP, user, &session, user_text_event("line one\u{0}line two")).await;
    let stored = fetch(&*sessions, APP, user, &session).await;
    let (response_kept, state_kept, text_kept) = match &stored {
        Ok(stored) => {
            let events = stored.events().all();
            let response_kept = events.iter().any(|event| {
                event.llm_response.content.as_ref().is_some_and(|content| {
                    content.parts.iter().any(|part| match part {
                        Part::FunctionResponse { function_response, .. } => {
                            contains_string(&function_response.response, BLOB_TEXT)
                        }
                        _ => false,
                    })
                })
            });
            let text_kept = events.iter().any(|event| {
                event.llm_response.content.as_ref().is_some_and(|content| {
                    content.parts.iter().any(|part| part.text() == Some("line one\u{0}line two"))
                })
            });
            (response_kept, stored.state().get("blob") == Some(json!(BLOB_STATE)), text_kept)
        }
        Err(_) => (false, false, false),
    };

    Ok(verdict(vec![
        (turned.is_ok(), format!("the turn whose tool returns U+0000 completes: {turned:?}")),
        (
            response_kept,
            "the stored function response holds the exact text with U+0000".to_string(),
        ),
        (state_kept, "the state value with U+0000 reads back unchanged".to_string()),
        (
            text_append.is_ok() && text_kept,
            format!(
                "a message with U+0000 is stored and read back: {:?}",
                text_append.map_err(|e| describe(&e))
            ),
        ),
    ]))
}

/// `list` returns the most recently updated session first, and `limit`/`offset` pages tile it.
async fn list_order_and_pages(ctx: &Ctx<'_>) -> anyhow::Result<Verdict> {
    let sessions = open_or_skip!(ctx);
    let model_server = ScriptedServer::start(agent_script()).await?;
    let http =
        HttpServer::start(build_agent(scripted_model(&model_server)?)?, sessions.clone()).await?;
    let user = unique("lister");
    // Seconds after `base` at which session `i` was last updated; not creation order.
    let offsets = [5, 11, 0, 7, 2, 9, 1, 10, 3, 8, 4, 6];
    let base = Utc::now().trunc_subsecs(6) + chrono::Duration::hours(1);
    let mut expected: Vec<(i64, String)> = Vec::new();
    for (i, offset) in offsets.iter().enumerate() {
        let session = format!("l{i:02}");
        sessions.create(create_request(APP, &user, &session, json!({}))).await?;
        let mut event = user_text_event("touch");
        event.timestamp = base + chrono::Duration::seconds(*offset);
        append(&*sessions, APP, &user, &session, event).await?;
        expected.push((*offset, session));
    }
    expected.sort_by_key(|(offset, _)| std::cmp::Reverse(*offset));
    let expected: Vec<String> = expected.into_iter().map(|(_, id)| id).collect();

    let list = |limit: Option<usize>, offset: Option<usize>| ListRequest {
        app_name: APP.to_string(),
        user_id: user.clone(),
        limit,
        offset,
    };
    let ids = |listed: Vec<Box<dyn Session>>| {
        listed.iter().map(|s| s.id().to_string()).collect::<Vec<_>>()
    };
    let all = ids(sessions.list(list(None, None)).await?);
    let mut paged = Vec::new();
    for offset in [0, 5, 10] {
        paged.extend(ids(sessions.list(list(Some(5), Some(offset))).await?));
    }
    let (status, body) = http.get(&format!("/api/apps/{APP}/users/{user}/sessions")).await?;
    let http_ids: Vec<String> = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|session| session.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();

    Ok(verdict(vec![
        (all == expected, format!("list is newest-updated first: {all:?}")),
        (paged == expected, format!("pages of 5 at offsets 0, 5, 10 tile the list: {paged:?}")),
        (
            status == 200 && http_ids == expected,
            format!(
                "GET /api/apps/{{app}}/users/{{user}}/sessions lists the same order ({status})"
            ),
        ),
    ]))
}

/// The model text in a `POST /api/run` response body.
fn response_text(body: &str) -> String {
    let events: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let mut text = String::new();
    for event in events.as_array().into_iter().flatten() {
        let parts = event.pointer("/content/parts").and_then(Value::as_array);
        for part in parts.into_iter().flatten() {
            if let Some(chunk) = part.get("text").and_then(Value::as_str) {
                text.push_str(chunk);
            }
        }
    }
    text
}

/// Live: a transient `get` failure between two real turns fails one request and the session
/// keeps the first turn, so the third request can answer from it.
async fn live_recovery(ctx: &Ctx<'_>) -> anyhow::Result<Verdict> {
    if ctx.backend != Backend::Memory {
        return Ok(Verdict::Skip("runs on the memory backend only".to_string()));
    }
    let Ok(key) = std::env::var("OPENAI_API_KEY") else {
        return Ok(Verdict::Skip("OPENAI_API_KEY is not set".to_string()));
    };
    let sessions = open_or_skip!(ctx);
    let faults = Faults::wrap(sessions.clone());
    let model: Arc<dyn Llm> = Arc::new(OpenAIClient::new(OpenAIConfig::new(key, LIVE_MODEL))?);
    let http = HttpServer::start(build_agent(model)?, faults.clone()).await?;
    let user = "live-user";
    let session = unique("live");
    sessions.create(create_request(APP, user, &session, json!({}))).await?;

    let first =
        http.run(false, user, &session, "My favourite colour is teal. Reply with just OK.").await?;
    faults.fail_next_get();
    let second =
        http.run(false, user, &session, "Are you still there? Reply with just yes.").await?;
    let third = http
        .run(false, user, &session, "What is my favourite colour? Reply with one word.")
        .await?;

    Ok(verdict(vec![
        (first.0 == 200, format!("turn 1 returns {}", first.0)),
        (second.0 == 503, format!("the request whose session read fails returns {}", second.0)),
        (
            third.0 == 200 && response_text(&third.1).to_lowercase().contains("teal"),
            format!("turn 3 answers from turn 1 ({}): {}", third.0, response_text(&third.1)),
        ),
    ]))
}

const SCENARIOS: &[&str] = &[
    "rewind_scoped_to_user",
    "rewind_keeps_initial_state",
    "duplicate_create_conflicts",
    "transient_get_keeps_session",
    "concurrent_first_requests",
    "unknown_session_not_found",
    "same_millisecond_order",
    "nul_round_trip",
    "list_order_and_pages",
    "live_recovery",
];

async fn run_scenario(name: &str, ctx: &Ctx<'_>) -> anyhow::Result<Verdict> {
    match name {
        "rewind_scoped_to_user" => rewind_scoped_to_user(ctx).await,
        "rewind_keeps_initial_state" => rewind_keeps_initial_state(ctx).await,
        "duplicate_create_conflicts" => duplicate_create_conflicts(ctx).await,
        "transient_get_keeps_session" => transient_get_keeps_session(ctx).await,
        "concurrent_first_requests" => concurrent_first_requests(ctx).await,
        "unknown_session_not_found" => unknown_session_not_found(ctx).await,
        "same_millisecond_order" => same_millisecond_order(ctx).await,
        "nul_round_trip" => nul_round_trip(ctx).await,
        "list_order_and_pages" => list_order_and_pages(ctx).await,
        "live_recovery" => live_recovery(ctx).await,
        other => anyhow::bail!("unknown scenario {other}; known: {}", SCENARIOS.join(", ")),
    }
}

fn load_env() {
    let env_file = std::env::var("ADK_ENV_FILE").unwrap_or_else(|_| ".env".to_string());
    let _ = dotenvy::from_path_override(&env_file);
}

#[tokio::main]
async fn main() -> ExitCode {
    load_env();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "error".into()),
        )
        .with_writer(std::io::stderr)
        .try_init();
    adk_core::ensure_crypto_provider();

    let mut scenarios: Vec<String> = SCENARIOS.iter().map(ToString::to_string).collect();
    let mut backends: Vec<Backend> = BACKENDS.to_vec();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        match (flag.as_str(), args.next()) {
            ("--scenario", Some(value)) => {
                scenarios = value.split(',').map(str::to_string).collect()
            }
            ("--backend", Some(value)) => {
                backends = value.split(',').filter_map(Backend::parse).collect();
            }
            _ => {}
        }
    }

    let dir = match tempfile::tempdir() {
        Ok(dir) => dir,
        Err(error) => {
            println!("cannot create a temporary directory: {error}");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "session integrity — {} scenario(s) × {} backend(s)\n",
        scenarios.len(),
        backends.len()
    );
    let (mut total, mut failures) = (0, 0);
    for name in &scenarios {
        for backend in &backends {
            total += 1;
            let ctx = Ctx { backend: *backend, dir: &dir };
            let result =
                tokio::time::timeout(Duration::from_secs(180), run_scenario(name, &ctx)).await;
            let (status, evidence) = match result {
                Ok(Ok(Verdict::Pass(evidence))) => ("PASS", evidence),
                Ok(Ok(Verdict::Skip(reason))) => ("SKIP", reason),
                Ok(Ok(Verdict::Fail(evidence))) => {
                    failures += 1;
                    ("FAIL", evidence)
                }
                Ok(Err(error)) => {
                    failures += 1;
                    ("FAIL", format!("scenario error: {error:#}"))
                }
                Err(_) => {
                    failures += 1;
                    ("FAIL", "timed out after 180s".into())
                }
            };
            println!("{status}  {name} [{}]\n      {evidence}", backend.name());
        }
    }
    println!("\n{total} run(s), {failures} failed");
    if failures == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
