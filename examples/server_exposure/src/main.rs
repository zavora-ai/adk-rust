//! Server and A2A exposure, validated against the real `adk-server` and `adk-cli` stack.
//!
//! Every scenario serves a real `LlmAgent` through `ServerBuilder` or the `Launcher` and talks
//! to it over HTTP with the real A2A clients. Live scenarios use Gemini and OpenAI; scripted
//! scenarios point the real `OpenAIClient` at a local Chat Completions upstream, or point the
//! real A2A clients at a local peer, for faults a live provider never produces.
//!
//! | Group | Asserts |
//! |-------|---------|
//! | A | `Launcher` and `run_serve` listen on loopback only by default |
//! | B | A cross-origin request, or one with a DNS-rebinding `Host`, is refused |
//! | C | A held or long-running tool call leaves the A2A task `input-required` |
//! | D | Model thinking never reaches an A2A peer |
//! | E | A truncated A2A stream is an error, and a split multi-byte character stays intact; only an administrator freezes or unfreezes the kill switch |
//! | F | An agent card whose RPC endpoint names another host is refused; a debug trace is returned to its owner only |
//! | G | An error response carries a provider's or a store's message only with `expose_error_details` |
//! | H | The agent registry authenticates every request; only a card's owner changes it |
//! | I | Background runs in open mode warn at mount, and keep each user's idempotency keys apart |

mod background;
mod debug_traces;
mod error_details;
mod governance_admin;
mod peers;
mod registry;
mod scripted;

use std::net::{IpAddr, SocketAddr};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_cli::Launcher;
use adk_cli::launcher::TelemetryConfig;
use adk_core::{
    Agent, Content, Llm, LlmRequest, LlmResponseStream, Part, SchemaAdapter, SessionId,
    SingleAgentLoader, Tool, ToolContext, UserId,
};
use adk_model::gemini::{ThinkingConfig, ThinkingLevel};
use adk_model::{GeminiModel, OpenAIClient, OpenAIConfig};
use adk_runner::Runner;
use adk_server::a2a::client::v1_client::A2aV1Client;
use adk_server::a2a::remote_agent::v1_remote::{RemoteA2aV1Agent, RemoteA2aV1Config};
use adk_server::a2a::v1::card::{CachedAgentCard, build_v1_agent_card};
use adk_server::a2a::v1::executor::V1Executor;
use adk_server::a2a::v1::jsonrpc_handler::jsonrpc_handler;
use adk_server::a2a::v1::push::NoOpPushNotificationSender;
use adk_server::a2a::v1::request_handler::RequestHandler;
use adk_server::a2a::v1::task_store::InMemoryTaskStore;
use adk_server::a2a::{A2aClient, Message, Role, Task, UpdateEvent};
use adk_server::{RemoteA2aAgent, ServerBuilder, ServerConfig};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use axum::Router;
use axum::routing::post;
use futures::StreamExt;
use serde_json::{Map, Value, json};

use crate::peers::MULTIBYTE_TEXT;
use crate::scripted::{SECRET_REASONING, ScriptedModel, THINKING_ANSWER};

const GEMINI_FAST: &str = "gemini-3.5-flash-lite";
const GEMINI_THINKING: &str = "gemini-3.7-flash";
const OPENAI_MODEL: &str = "gpt-5.6-terra";
const REBIND_HOST: &str = "rebind.attacker.example";

#[derive(Clone, Copy)]
enum Kind {
    /// Calls a live provider.
    Live,
    /// Drives a scripted upstream or peer.
    Scripted,
    /// Exercises the server itself; no model call.
    Local,
}

enum Verdict {
    Pass,
    Fail,
    Skip,
}

struct Outcome {
    id: &'static str,
    kind: Kind,
    verdict: Verdict,
    detail: String,
}

fn outcome(id: &'static str, kind: Kind, pass: bool, detail: impl Into<String>) -> Outcome {
    Outcome {
        id,
        kind,
        verdict: if pass { Verdict::Pass } else { Verdict::Fail },
        detail: detail.into(),
    }
}

fn skip(id: &'static str, kind: Kind, detail: impl Into<String>) -> Outcome {
    Outcome { id, kind, verdict: Verdict::Skip, detail: detail.into() }
}

fn errored(id: &'static str, kind: Kind, error: anyhow::Error) -> Outcome {
    outcome(id, kind, false, format!("scenario error: {error:#}"))
}

struct Keys {
    gemini: Option<String>,
    openai: Option<String>,
}

fn load_env() {
    let candidates = match std::env::var("ADK_ENV_FILE") {
        Ok(path) => vec![path],
        Err(_) => vec![
            concat!(env!("CARGO_MANIFEST_DIR"), "/.env").to_string(),
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env").to_string(),
        ],
    };
    if let Some(path) = candidates.iter().find(|path| std::path::Path::new(path).exists()) {
        let _ = dotenvy::from_path_override(path);
    }
}

fn keys() -> Keys {
    let non_empty = |name: &str| std::env::var(name).ok().filter(|value| !value.trim().is_empty());
    Keys {
        gemini: non_empty("GEMINI_API_KEY").or_else(|| non_empty("GOOGLE_API_KEY")),
        openai: non_empty("OPENAI_API_KEY"),
    }
}

// ── Shared fixtures ──────────────────────────────────────────────────────────

/// A tool with one string argument. `delete_record` is held for confirmation by the agent;
/// `start_export` is long-running.
struct RecordTool {
    name: &'static str,
    description: &'static str,
    argument: &'static str,
    long_running: bool,
}

#[async_trait]
impl Tool for RecordTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        self.description
    }

    fn parameters_schema(&self) -> Option<Value> {
        let mut properties = Map::new();
        properties.insert(self.argument.to_string(), json!({"type": "string"}));
        Some(json!({"type": "object", "properties": properties, "required": [self.argument]}))
    }

    fn is_long_running(&self) -> bool {
        self.long_running
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        Ok(if self.long_running {
            json!({"status": "pending", "request": args})
        } else {
            json!({"deleted": args})
        })
    }
}

fn ops_agent(model: Arc<dyn Llm>) -> anyhow::Result<Arc<dyn Agent>> {
    Ok(Arc::new(
        LlmAgentBuilder::new("ops_agent")
            .description("Deletes records and starts exports")
            .instruction(
                "You are ops_agent. When the user asks to delete a record, call delete_record \
                 with the record id immediately. When the user asks to start an export, call \
                 start_export with the export id immediately. Never ask for confirmation in text.",
            )
            .model(model)
            .tool(Arc::new(RecordTool {
                name: "delete_record",
                description: "Deletes a record by id",
                argument: "record_id",
                long_running: false,
            }))
            .tool(Arc::new(RecordTool {
                name: "start_export",
                description: "Starts a long-running export by id",
                argument: "export_id",
                long_running: true,
            }))
            .require_tool_confirmation("delete_record")
            .build()?,
    ))
}

fn plain_agent(
    name: &str,
    instruction: &str,
    model: Arc<dyn Llm>,
) -> anyhow::Result<Arc<dyn Agent>> {
    Ok(Arc::new(
        LlmAgentBuilder::new(name)
            .description("Answers briefly")
            .instruction(instruction)
            .model(model)
            .build()?,
    ))
}

fn scripted_model(server: &ScriptedModel) -> anyhow::Result<Arc<dyn Llm>> {
    Ok(Arc::new(OpenAIClient::compatible("scripted-key", &server.base_url, "scripted-model")?))
}

/// Records every thought summary the wrapped model returns, so a scenario can check that
/// none of it reached an A2A peer.
struct CaptureThoughts {
    inner: Arc<dyn Llm>,
    thoughts: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Llm for CaptureThoughts {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn schema_adapter(&self) -> &dyn SchemaAdapter {
        self.inner.schema_adapter()
    }

    async fn generate_content(
        &self,
        req: LlmRequest,
        stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let responses = self.inner.generate_content(req, stream).await?;
        let thoughts = self.thoughts.clone();
        Ok(Box::pin(responses.inspect(move |item| {
            if let Ok(response) = item
                && let Some(content) = &response.content
            {
                for part in &content.parts {
                    if let Part::Thinking { thinking, .. } = part {
                        thoughts.lock().unwrap().push(thinking.clone());
                    }
                }
            }
        })))
    }
}

/// Serves the router built for its own base URL on 127.0.0.1.
async fn serve(
    build: impl FnOnce(&str) -> Router,
) -> anyhow::Result<(String, tokio::task::JoinHandle<()>)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let app = build(&base);
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok((base, task))
}

/// The v1 JSON-RPC route for `agent`, mounted at `/v1/a2a`.
fn v1_routes(base: &str, agent: Arc<dyn Agent>, sessions: Arc<dyn SessionService>) -> Router {
    let store = Arc::new(InMemoryTaskStore::new());
    let executor = Arc::new(V1Executor::new(store.clone()));
    let card = build_v1_agent_card(
        agent.name(),
        agent.description(),
        &format!("{base}/v1/a2a"),
        "1.0.0",
        vec![],
        a2a_protocol_types::AgentCapabilities::none(),
    );
    let cached = Arc::new(tokio::sync::RwLock::new(CachedAgentCard::new(card)));
    let runner_config = Arc::new(
        Runner::builder()
            .app_name(agent.name())
            .agent(agent)
            .session_service(sessions)
            .build_config(),
    );
    let handler = Arc::new(RequestHandler::with_runner(
        executor,
        store,
        Arc::new(NoOpPushNotificationSender),
        cached,
        runner_config,
    ));
    Router::new().route("/v1/a2a", post(jsonrpc_handler)).with_state(handler)
}

/// Serves `agent` with the built-in A2A routes and the v1 JSON-RPC route, default security.
async fn serve_a2a(agent: Arc<dyn Agent>) -> anyhow::Result<(String, tokio::task::JoinHandle<()>)> {
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    serve(|base| {
        // Thinking models can take longer than the 30-second default.
        let config =
            ServerConfig::new(Arc::new(SingleAgentLoader::new(agent.clone())), sessions.clone())
                .with_request_timeout(Duration::from_secs(120));
        ServerBuilder::new(config)
            .with_a2a(base)
            .add_root_routes(v1_routes(base, agent, sessions))
            .build()
    })
    .await
}

fn a2a_text(text: &str) -> Message {
    Message::builder()
        .role(Role::User)
        .parts(vec![adk_server::a2a::Part::text(text.to_string())])
        .build()
}

fn v1_text(text: &str) -> a2a_protocol_types::Message {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    a2a_protocol_types::Message {
        id: a2a_protocol_types::MessageId::new(format!("msg-{nanos}")),
        role: a2a_protocol_types::MessageRole::User,
        parts: vec![a2a_protocol_types::Part::text(text)],
        task_id: None,
        context_id: None,
        reference_task_ids: None,
        extensions: None,
        metadata: None,
    }
}

/// The wire name of a legacy task state, such as `input-required`.
fn state_name(state: &adk_server::a2a::TaskState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn task_texts(task: &Task) -> Vec<String> {
    task.artifacts
        .iter()
        .flatten()
        .flat_map(|artifact| artifact.parts.iter())
        .filter_map(|part| match part {
            adk_server::a2a::Part::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn task_called(task: &Task, tool: &str) -> bool {
    task.artifacts.iter().flatten().flat_map(|artifact| artifact.parts.iter()).any(|part| {
        matches!(part, adk_server::a2a::Part::Data { data, .. }
            if data.get("function_call").and_then(|call| call.get("name")).and_then(Value::as_str) == Some(tool))
    })
}

async fn a2a_send(base: &str, text: &str) -> anyhow::Result<Task> {
    let client = A2aClient::from_url(base).await?;
    let response = client.send_message(a2a_text(text)).await?;
    if let Some(error) = response.error {
        anyhow::bail!("JSON-RPC error {}: {}", error.code, error.message);
    }
    let result = response.result.ok_or_else(|| anyhow::anyhow!("no result"))?;
    Ok(serde_json::from_value(result)?)
}

/// Runs `agent` on its own runner, as a parent agent would, and returns its events.
async fn run_client(agent: Arc<dyn Agent>, text: &str) -> anyhow::Result<Vec<adk_core::Event>> {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "client".to_string(),
            user_id: "user".to_string(),
            session_id: Some("session".to_string()),
            state: Default::default(),
        })
        .await?;
    let runner =
        Runner::builder().app_name("client").agent(agent).session_service(sessions).build()?;
    let mut events = runner
        .run(UserId::new("user")?, SessionId::new("session")?, Content::new("user").with_text(text))
        .await?;
    let mut collected = Vec::new();
    while let Some(event) = tokio::time::timeout(Duration::from_secs(90), events.next()).await? {
        collected.push(event?);
    }
    Ok(collected)
}

fn event_text(events: &[adk_core::Event]) -> String {
    events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter_map(Part::text)
        .collect()
}

fn event_errors(events: &[adk_core::Event]) -> Vec<String> {
    events.iter().filter_map(|event| event.llm_response.error_message.clone()).collect()
}

fn http() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(90)).build().expect("client")
}

// ── A: default bind ──────────────────────────────────────────────────────────

/// The first non-loopback address of this host, found without sending a packet.
fn lan_ip() -> Option<IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

fn free_port() -> anyhow::Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

async fn listening(address: SocketAddr) -> bool {
    tokio::time::timeout(Duration::from_millis(500), tokio::net::TcpStream::connect(address))
        .await
        .is_ok_and(|connected| connected.is_ok())
}

/// Starts a server with `start(port)` and reports whether it answers on a LAN address.
async fn reachable_from_lan<F, Fut>(lan: IpAddr, start: F) -> anyhow::Result<bool>
where
    F: FnOnce(u16) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let port = free_port()?;
    let task = tokio::spawn(start(port));
    let loopback = SocketAddr::from(([127, 0, 0, 1], port));
    let mut up = false;
    for _ in 0..100 {
        if listening(loopback).await {
            up = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let reachable = up && listening(SocketAddr::new(lan, port)).await;
    task.abort();
    if !up {
        anyhow::bail!("server never listened on 127.0.0.1:{port}");
    }
    Ok(reachable)
}

async fn group_a(model: &ScriptedModel, out: &mut Vec<Outcome>) {
    let Some(lan) = lan_ip() else {
        out.push(skip("A1 launcher-default-bind", Kind::Local, "no non-loopback address"));
        out.push(skip("A2 run-serve-default-bind", Kind::Local, "no non-loopback address"));
        return;
    };
    let agent = match scripted_model(model)
        .and_then(|m| plain_agent("bind_probe", "You are bind_probe.", m))
    {
        Ok(agent) => agent,
        Err(error) => {
            out.push(errored("A1 launcher-default-bind", Kind::Local, error));
            return;
        }
    };

    let launcher_agent = agent.clone();
    let launcher = reachable_from_lan(lan, move |port| async move {
        let _ = Launcher::new(launcher_agent)
            .with_telemetry(TelemetryConfig::None)
            .run_serve_directly(port)
            .await;
    })
    .await;
    out.push(match launcher {
        Ok(reachable) => outcome(
            "A1 launcher-default-bind",
            Kind::Local,
            !reachable,
            if reachable {
                format!("Launcher::run_serve_directly answered on {lan}")
            } else {
                "loopback only; LAN connect refused".to_string()
            },
        ),
        Err(error) => errored("A1 launcher-default-bind", Kind::Local, error),
    });

    let serve_agent = agent;
    let legacy = reachable_from_lan(lan, move |port| async move {
        let _ =
            adk_cli::serve::run_serve(Arc::new(SingleAgentLoader::new(serve_agent)), port).await;
    })
    .await;
    out.push(match legacy {
        Ok(reachable) => outcome(
            "A2 run-serve-default-bind",
            Kind::Local,
            !reachable,
            if reachable {
                format!("adk_cli::serve::run_serve answered on {lan}")
            } else {
                "loopback only; LAN connect refused".to_string()
            },
        ),
        Err(error) => errored("A2 run-serve-default-bind", Kind::Local, error),
    });
}

// ── B: cross-origin and DNS rebinding ────────────────────────────────────────

fn run_body(app: &str, user: &str, session: &str, text: &str) -> Value {
    json!({
        "appName": app,
        "userId": user,
        "sessionId": session,
        "newMessage": {"role": "user", "parts": [{"text": text}]},
    })
}

async fn group_b(keys: &Keys, model: &ScriptedModel, out: &mut Vec<Outcome>) {
    let (kind, agent_model): (Kind, anyhow::Result<Arc<dyn Llm>>) = match &keys.gemini {
        Some(key) => (
            Kind::Live,
            GeminiModel::new(key, GEMINI_FAST)
                .map(|m| Arc::new(m) as Arc<dyn Llm>)
                .map_err(Into::into),
        ),
        None => (Kind::Scripted, scripted_model(model)),
    };
    let app = "rebind_target";
    let agent = match agent_model
        .and_then(|m| plain_agent(app, "You are rebind_target. Reply with one short sentence.", m))
    {
        Ok(agent) => agent,
        Err(error) => {
            out.push(errored("B1 loopback-control", kind, error));
            return;
        }
    };
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let served = serve(|_| {
        ServerBuilder::new(ServerConfig::new(
            Arc::new(SingleAgentLoader::new(agent.clone())),
            sessions.clone(),
        ))
        .build()
    })
    .await;
    let (base, task) = match served {
        Ok(served) => served,
        Err(error) => {
            out.push(errored("B1 loopback-control", kind, error));
            return;
        }
    };
    let port = base.rsplit(':').next().unwrap_or_default().to_string();
    let rebind_host = format!("{REBIND_HOST}:{port}");
    let rebind_origin = format!("http://{rebind_host}");
    let client = http();

    // B1: the same request from loopback reaches the agent.
    let control = async {
        let seeded = client
            .post(format!("{base}/api/apps/{app}/users/alice/sessions/s-alice"))
            .json(&json!({}))
            .send()
            .await?;
        anyhow::ensure!(
            seeded.status().is_success(),
            "seeding alice's session: {}",
            seeded.status()
        );
        let response = client
            .post(format!("{base}/api/run"))
            .json(&run_body(app, "bob", "s-bob", "Say hello."))
            .send()
            .await?;
        let status = response.status();
        let events: Vec<adk_core::Event> = response.json().await.unwrap_or_default();
        Ok::<_, anyhow::Error>((status, event_text(&events)))
    }
    .await;
    out.push(match control {
        Ok((status, text)) => outcome(
            "B1 loopback-control",
            kind,
            status.is_success() && !text.trim().is_empty(),
            format!("HTTP {status}, agent said {:?}", truncate(&text, 60)),
        ),
        Err(error) => errored("B1 loopback-control", kind, error),
    });

    // B2: a page on a rebinding domain posts to /api/run.
    let rebind_run = async {
        let response = client
            .post(format!("{base}/api/run"))
            .header("host", &rebind_host)
            .header("origin", &rebind_origin)
            .json(&run_body(app, "mallory", "s-mallory", "Say hello."))
            .send()
            .await?;
        let status = response.status();
        let events: Vec<adk_core::Event> = response.json().await.unwrap_or_default();
        Ok::<_, anyhow::Error>((status, events.len()))
    }
    .await;
    out.push(match rebind_run {
        Ok((status, events)) => outcome(
            "B2 rebind-run-refused",
            kind,
            status.is_client_error() && events == 0,
            if status.is_success() {
                format!("HTTP {status}: the agent ran for Host {rebind_host} ({events} events)")
            } else {
                format!("HTTP {status}")
            },
        ),
        Err(error) => errored("B2 rebind-run-refused", kind, error),
    });

    // B3: the same page reads another user's sessions.
    let rebind_read = async {
        let response = client
            .get(format!("{base}/api/apps/{app}/users/alice/sessions"))
            .header("host", &rebind_host)
            .header("origin", &rebind_origin)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        Ok::<_, anyhow::Error>((status, body.contains("s-alice")))
    }
    .await;
    out.push(match rebind_read {
        Ok((status, leaked)) => outcome(
            "B3 rebind-session-read-refused",
            Kind::Local,
            status.is_client_error() && !leaked,
            if leaked {
                format!("HTTP {status}: alice's session s-alice listed for Host {rebind_host}")
            } else {
                format!("HTTP {status}")
            },
        ),
        Err(error) => errored("B3 rebind-session-read-refused", Kind::Local, error),
    });

    // B4: a CORS preflight from an unrelated origin.
    let preflight = async {
        let response = client
            .request(reqwest::Method::OPTIONS, format!("{base}/api/run"))
            .header("origin", "https://evil.example")
            .header("access-control-request-method", "POST")
            .header("access-control-request-headers", "content-type")
            .send()
            .await?;
        Ok::<_, anyhow::Error>(
            response
                .headers()
                .get("access-control-allow-origin")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string),
        )
    }
    .await;
    out.push(match preflight {
        Ok(allowed) => outcome(
            "B4 cors-preflight-refused",
            Kind::Local,
            allowed.is_none(),
            match allowed {
                Some(value) => format!("access-control-allow-origin: {value}"),
                None => "no access-control-allow-origin".to_string(),
            },
        ),
        Err(error) => errored("B4 cors-preflight-refused", Kind::Local, error),
    });
    task.abort();

    // B5: the Launcher's app has the same defaults.
    let launcher = async {
        let agent =
            plain_agent("launcher_target", "You are launcher_target.", scripted_model(model)?)?;
        let router = Launcher::new(agent).with_telemetry(TelemetryConfig::None).build_app()?;
        let (base, task) = serve(|_| router).await?;
        let port = base.rsplit(':').next().unwrap_or_default().to_string();
        let response = client
            .get(format!("{base}/api/apps/launcher_target/users/alice/sessions"))
            .header("host", format!("{REBIND_HOST}:{port}"))
            .send()
            .await?;
        task.abort();
        Ok::<_, anyhow::Error>(response.status())
    }
    .await;
    out.push(match launcher {
        Ok(status) => outcome(
            "B5 launcher-app-rebind-refused",
            Kind::Local,
            status.is_client_error(),
            format!("HTTP {status} for Host {REBIND_HOST}"),
        ),
        Err(error) => errored("B5 launcher-app-rebind-refused", Kind::Local, error),
    });
}

fn truncate(text: &str, max: usize) -> String {
    let mut short: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        short.push('…');
    }
    short
}

// ── C: input-required ────────────────────────────────────────────────────────

async fn group_c(keys: &Keys, model: &ScriptedModel, out: &mut Vec<Outcome>) {
    let served = async { serve_a2a(ops_agent(scripted_model(model)?)?).await }.await;
    let (base, task) = match served {
        Ok(served) => served,
        Err(error) => {
            out.push(errored("C1 held-call-input-required", Kind::Scripted, error));
            return;
        }
    };

    let held = a2a_send(&base, "Please delete record R-17.").await;
    out.push(match held {
        Ok(task) => {
            let state = state_name(&task.status.state);
            outcome(
                "C1 held-call-input-required",
                Kind::Scripted,
                state == "input-required",
                format!("state {state}"),
            )
        }
        Err(error) => errored("C1 held-call-input-required", Kind::Scripted, error),
    });

    let streamed = async {
        let client = A2aClient::from_url(&base).await?;
        let mut stream =
            client.send_streaming_message(a2a_text("Please delete record R-17 again.")).await?;
        let mut last_state = None;
        let mut errors = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(UpdateEvent::TaskStatusUpdate(update)) => {
                    last_state = Some(state_name(&update.status.state))
                }
                Ok(UpdateEvent::TaskArtifactUpdate(_)) => {}
                Err(error) => errors.push(error.to_string()),
            }
        }
        Ok::<_, anyhow::Error>((last_state.unwrap_or_default(), errors))
    }
    .await;
    out.push(match streamed {
        Ok((state, errors)) => outcome(
            "C2 held-call-input-required-stream",
            Kind::Scripted,
            state == "input-required" && errors.is_empty(),
            format!("final state {state}, stream errors {errors:?}"),
        ),
        Err(error) => errored("C2 held-call-input-required-stream", Kind::Scripted, error),
    });

    let long_running = a2a_send(&base, "Please start export E-9.").await;
    out.push(match long_running {
        Ok(task) => {
            let state = state_name(&task.status.state);
            outcome(
                "C3 long-running-input-required",
                Kind::Scripted,
                state == "input-required",
                format!("state {state}"),
            )
        }
        Err(error) => errored("C3 long-running-input-required", Kind::Scripted, error),
    });

    let v1 = async {
        let card = build_v1_agent_card(
            "ops_agent",
            "ops",
            &format!("{base}/v1/a2a"),
            "1.0.0",
            vec![],
            a2a_protocol_types::AgentCapabilities::none(),
        );
        let task =
            A2aV1Client::new(card).send_message(v1_text("Please delete record R-17.")).await?;
        Ok::<_, anyhow::Error>(task.status.state)
    }
    .await;
    out.push(match v1 {
        Ok(state) => outcome(
            "C4 held-call-input-required-v1",
            Kind::Scripted,
            state == a2a_protocol_types::task::TaskState::InputRequired,
            format!("state {state:?}"),
        ),
        Err(error) => errored("C4 held-call-input-required-v1", Kind::Scripted, error),
    });
    task.abort();

    let Some(key) = &keys.openai else {
        out.push(skip("C5 held-call-input-required-live", Kind::Live, "OPENAI_API_KEY not set"));
        return;
    };
    let live = async {
        let model: Arc<dyn Llm> =
            Arc::new(OpenAIClient::new(OpenAIConfig::new(key, OPENAI_MODEL))?);
        let (base, task) = serve_a2a(ops_agent(model)?).await?;
        let result = a2a_send(&base, "Delete record R-17 now.").await;
        task.abort();
        result
    }
    .await;
    out.push(match live {
        Ok(task) => {
            let state = state_name(&task.status.state);
            let called = task_called(&task, "delete_record");
            outcome(
                "C5 held-call-input-required-live",
                Kind::Live,
                called && state == "input-required",
                format!("{OPENAI_MODEL} called delete_record: {called}; state {state}"),
            )
        }
        Err(error) => errored("C5 held-call-input-required-live", Kind::Live, error),
    });
}

// ── D: thinking stays private ────────────────────────────────────────────────

async fn group_d(keys: &Keys, model: &ScriptedModel, out: &mut Vec<Outcome>) {
    let scripted = async {
        let agent = plain_agent("thinker", "You are thinker.", scripted_model(model)?)?;
        let (base, task) = serve_a2a(agent).await?;
        let sent = a2a_send(&base, "Think it over: what is six times seven?").await?;
        let client = A2aClient::from_url(&base).await?;
        let mut stream =
            client.send_streaming_message(a2a_text("Think again: six times seven?")).await?;
        let mut streamed = String::new();
        while let Some(item) = stream.next().await {
            if let Ok(UpdateEvent::TaskArtifactUpdate(update)) = item {
                for part in &update.artifact.parts {
                    if let adk_server::a2a::Part::Text { text, .. } = part {
                        streamed.push_str(text);
                    }
                }
            }
        }
        task.abort();
        Ok::<_, anyhow::Error>((task_texts(&sent).concat(), streamed))
    }
    .await;
    out.push(match scripted {
        Ok((sent, streamed)) => {
            let leaked = sent.contains(SECRET_REASONING) || streamed.contains(SECRET_REASONING);
            let answered = sent.contains(THINKING_ANSWER) && streamed.contains(THINKING_ANSWER);
            outcome(
                "D1 thinking-private",
                Kind::Scripted,
                !leaked && answered,
                format!("reasoning leaked: {leaked}; answer delivered: {answered}"),
            )
        }
        Err(error) => errored("D1 thinking-private", Kind::Scripted, error),
    });

    let Some(key) = &keys.gemini else {
        out.push(skip("D2 thinking-private-live", Kind::Live, "GEMINI_API_KEY not set"));
        return;
    };
    let live = async {
        let thoughts = Arc::new(Mutex::new(Vec::new()));
        let gemini = GeminiModel::new(key, GEMINI_THINKING)?.with_thinking_config(
            ThinkingConfig::new().with_thoughts_included(true).with_thinking_level(ThinkingLevel::Low),
        );
        let model: Arc<dyn Llm> = Arc::new(CaptureThoughts { inner: Arc::new(gemini), thoughts: thoughts.clone() });
        let agent = plain_agent("live_thinker", "You are live_thinker. Answer in one short sentence.", model)?;
        let (base, task) = serve_a2a(agent).await?;
        // A thought summary is optional for the model, so ask up to three times for one.
        let mut answer = String::new();
        for _ in 0..3 {
            let sent = a2a_send(
                &base,
                "A bat and a ball cost $1.10 in total. The bat costs $1.00 more than the ball. How much is the ball?",
            )
            .await;
            answer = match sent {
                Ok(task) => task_texts(&task).concat(),
                Err(error) => {
                    task.abort();
                    return Err(error);
                }
            };
            if thoughts.lock().unwrap().iter().any(|thought| !thought.trim().is_empty()) {
                break;
            }
        }
        task.abort();
        let thoughts = thoughts.lock().unwrap().clone();
        Ok::<_, anyhow::Error>((answer, thoughts))
    }
    .await;
    out.push(match live {
        Ok((_, thoughts)) if thoughts.iter().all(|thought| thought.trim().is_empty()) => {
            skip("D2 thinking-private-live", Kind::Live, format!("{GEMINI_THINKING} returned no thought summary"))
        }
        Ok((answer, thoughts)) => {
            let leaked = thoughts.iter().filter(|thought| !thought.trim().is_empty()).any(|thought| {
                let probe: String = thought.trim().chars().take(40).collect();
                answer.contains(&probe)
            });
            outcome(
                "D2 thinking-private-live",
                Kind::Live,
                !leaked && !answer.trim().is_empty(),
                format!(
                    "{GEMINI_THINKING}: {} thought part(s); reached the peer: {leaked}; answer {:?}",
                    thoughts.len(),
                    truncate(&answer, 50)
                ),
            )
        }
        Err(error) => errored("D2 thinking-private-live", Kind::Live, error),
    });
}

// ── E: strict A2A streams ────────────────────────────────────────────────────

fn stream_outcomes(
    out: &mut Vec<Outcome>,
    ids: (&'static str, &'static str),
    events: anyhow::Result<Vec<adk_core::Event>>,
) {
    match events {
        Ok(events) => {
            let text = event_text(&events);
            let errors = event_errors(&events);
            out.push(outcome(
                ids.0,
                Kind::Scripted,
                text.contains(MULTIBYTE_TEXT) && !text.contains('\u{FFFD}'),
                format!("received {:?}", text),
            ));
            out.push(outcome(
                ids.1,
                Kind::Scripted,
                !errors.is_empty(),
                if errors.is_empty() {
                    "stream ended without a final status and no error was reported".to_string()
                } else {
                    format!("error: {}", truncate(&errors.join("; "), 120))
                },
            ));
        }
        Err(error) => {
            out.push(errored(ids.0, Kind::Scripted, anyhow::anyhow!("{error:#}")));
            out.push(errored(ids.1, Kind::Scripted, error));
        }
    }
}

async fn group_e(keys: &Keys, out: &mut Vec<Outcome>) {
    let legacy = async {
        let peer = peers::truncating_legacy().await?;
        let agent = RemoteA2aAgent::builder("legacy_peer")
            .description("scripted peer")
            .agent_url(&peer.base_url)
            .streaming(true)
            .build()?;
        run_client(Arc::new(agent), "Greet me.").await
    }
    .await;
    stream_outcomes(out, ("E1 split-char-intact", "E2 truncated-stream-error"), legacy);

    let v1 = async {
        let peer = peers::truncating_v1().await?;
        let card = A2aV1Client::resolve_agent_card(&peer.base_url).await?;
        let agent = RemoteA2aV1Agent::new(RemoteA2aV1Config {
            name: "v1_peer".to_string(),
            description: "scripted v1 peer".to_string(),
            agent_card: card,
            streaming: Some(true),
            credential_broker: None,
            credential_scopes: vec![],
        });
        run_client(Arc::new(agent), "Greet me.").await
    }
    .await;
    stream_outcomes(out, ("E3 split-char-intact-v1", "E4 truncated-stream-error-v1"), v1);

    let Some(key) = &keys.gemini else {
        out.push(skip("E5 a2a-stream-live", Kind::Live, "GEMINI_API_KEY not set"));
        return;
    };
    let live = async {
        let model: Arc<dyn Llm> = Arc::new(GeminiModel::new(key, GEMINI_FAST)?);
        let agent = plain_agent(
            "echo_agent",
            "You are echo_agent. Reply with exactly the text the user asks for and nothing else.",
            model,
        )?;
        let (base, task) = serve_a2a(agent).await?;
        let remote = RemoteA2aAgent::builder("echo_remote")
            .description("live A2A peer")
            .agent_url(&base)
            .streaming(true)
            .build()?;
        let remote: Arc<dyn Agent> = Arc::new(remote);
        let prompt = format!("Reply with exactly: {MULTIBYTE_TEXT}");
        let mut events = run_client(remote.clone(), &prompt).await;
        // A free-tier key can hit a rate limit after the scenarios before this one.
        for _ in 0..2 {
            match &events {
                Ok(collected) if event_text(collected).contains("rate_limited") => {
                    tokio::time::sleep(Duration::from_secs(20)).await;
                    events = run_client(remote.clone(), &prompt).await;
                }
                _ => break,
            }
        }
        task.abort();
        events
    }
    .await;
    out.push(match live {
        Ok(events) => {
            let text = event_text(&events);
            let errors = event_errors(&events);
            outcome(
                "E5 a2a-stream-live",
                Kind::Live,
                errors.is_empty()
                    && text.contains("Köln")
                    && text.contains('☕')
                    && !text.contains('\u{FFFD}'),
                format!("{GEMINI_FAST} over A2A: {:?}; errors {errors:?}", truncate(&text, 60)),
            )
        }
        Err(error) => errored("E5 a2a-stream-live", Kind::Live, error),
    });
}

// ── F: agent card endpoints ──────────────────────────────────────────────────

async fn group_f(out: &mut Vec<Outcome>) {
    let legacy = async {
        let sink = peers::Sink::start().await?;
        let peer = peers::hostile_card(format!("{}/a2a", sink.peer.base_url), false).await?;
        let agent = RemoteA2aAgent::builder("hostile_peer")
            .description("card points elsewhere")
            .agent_url(&peer.base_url)
            .streaming(false)
            .build()?;
        let events = run_client(Arc::new(agent), "my account number is 12345").await?;
        Ok::<_, anyhow::Error>((sink.received(), event_errors(&events), sink.peer.base_url.clone()))
    }
    .await;
    out.push(match legacy {
        Ok((received, errors, sink)) => outcome(
            "F1 hostile-card-refused",
            Kind::Scripted,
            received == 0 && !errors.is_empty(),
            if received > 0 {
                format!("the message was sent to {sink}, the host the card named")
            } else {
                format!("refused: {}", truncate(&errors.join("; "), 120))
            },
        ),
        Err(error) => errored("F1 hostile-card-refused", Kind::Scripted, error),
    });

    let v1 = async {
        let sink = peers::Sink::start().await?;
        let peer = peers::hostile_card(format!("{}/a2a", sink.peer.base_url), true).await?;
        let card = match A2aV1Client::resolve_agent_card(&peer.base_url).await {
            Ok(card) => card,
            Err(error) => {
                return Ok::<_, anyhow::Error>((0, format!("refused at resolution: {error}")));
            }
        };
        let agent = RemoteA2aV1Agent::new(RemoteA2aV1Config {
            name: "hostile_v1_peer".to_string(),
            description: "card points elsewhere".to_string(),
            agent_card: card,
            streaming: Some(false),
            credential_broker: None,
            credential_scopes: vec![],
        });
        run_client(Arc::new(agent), "my account number is 12345").await?;
        Ok((sink.received(), format!("the message was sent to {}", sink.peer.base_url)))
    }
    .await;
    out.push(match v1 {
        Ok((received, detail)) => {
            outcome("F2 hostile-card-refused-v1", Kind::Scripted, received == 0, detail)
        }
        Err(error) => errored("F2 hostile-card-refused-v1", Kind::Scripted, error),
    });
}

#[tokio::main]
async fn main() -> ExitCode {
    load_env();
    adk_core::ensure_crypto_provider();
    let keys = keys();
    let model = match ScriptedModel::start().await {
        Ok(model) => model,
        Err(error) => {
            eprintln!("could not start the scripted model: {error:#}");
            return ExitCode::FAILURE;
        }
    };

    let mut out = Vec::new();
    group_a(&model, &mut out).await;
    group_b(&keys, &model, &mut out).await;
    group_c(&keys, &model, &mut out).await;
    group_d(&keys, &model, &mut out).await;
    group_e(&keys, &mut out).await;
    governance_admin::group_e_governance_admin(&mut out).await;
    group_f(&mut out).await;
    debug_traces::group_f_debug_traces(&mut out).await;
    error_details::group_g(&mut out).await;
    registry::group_h(&mut out).await;
    background::group_i(&model, &mut out).await;

    println!("\n| Scenario | Kind | Result | Detail |");
    println!("|----------|------|--------|--------|");
    let mut failed = 0;
    for outcome in &out {
        let kind = match outcome.kind {
            Kind::Live => "live",
            Kind::Scripted => "scripted",
            Kind::Local => "local",
        };
        let verdict = match outcome.verdict {
            Verdict::Pass => "PASS",
            Verdict::Fail => {
                failed += 1;
                "FAIL"
            }
            Verdict::Skip => "SKIP",
        };
        println!(
            "| {} | {kind} | {verdict} | {} |",
            outcome.id,
            outcome.detail.replace('|', "\\|")
        );
    }
    println!("\n{} scenario(s), {failed} failed", out.len());
    if failed == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
