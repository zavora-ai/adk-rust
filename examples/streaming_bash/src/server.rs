//! Web UI for the streaming-bash demo.
//!
//! Architecture — **server-owned agent, thin browser**:
//!
//! ```text
//!   browser ──prompt (JSON over WS)──▶ Rust /ws handler
//!   browser ◀──model text + tool ─────  LlmAgent + DevTools (Gemini)
//!            calls/progress/results      ├─ bash      → streams stdout/stderr
//!            (events)                    ├─ read_file → one-shot result
//!                                        ├─ grep      → one-shot result
//!                                        └─ glob      → one-shot result
//! ```
//!
//! Everything the browser renders rides **one** `EventStream`. For each event the
//! server forwards a typed frame derived from first-class accessors:
//!
//! - [`Event::tool_calls`] → `tool_call` (a tool started; show its card)
//! - [`Event::tool_progress_stream`] → `terminal` (a live stdout/stderr chunk)
//! - [`Event::tool_results`] → `tool_result` (the tool finished; render output)
//! - plain text parts → `token` (the model's reply)
//!
//! This is the point of the demo: **every** tool — streaming (`bash`) or
//! one-shot (`read_file`/`grep`/`glob`) — surfaces its output generically,
//! because results are first-class events, not a bash-only side channel.

use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::identity::{SessionId, UserId};
use adk_core::{Agent, Content, Llm, Part};
use adk_devtools::{BashTool, GlobTool, GrepTool, ReadFileTool, Workspace};
use adk_model::GeminiModel;
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use axum::{
    Router,
    extract::State,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::{Html, IntoResponse},
    routing::get,
};
use futures::stream::SplitSink;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tower_http::cors::CorsLayer;
use tracing::{info, warn};

const APP_NAME: &str = "streaming-bash";
const USER_ID: &str = "user";

const INSTRUCTION: &str = "You are a developer assistant with a toolbox: `bash` (run shell \
commands; streams output live), `read_file` (read a file's contents), `grep` (search file \
contents), and `glob` (find files by pattern). Pick the most appropriate tool for each \
request and use it — never claim a result without calling a tool first. Prefer `read_file` \
for showing a file, `grep` for searching, `glob` for finding files by name, and `bash` for \
anything else (system info, counting, multi-step shell work). Keep any explanation to one \
short sentence before calling the tool, then summarize the result briefly afterward.";

/// Shared app state: one runner reused across browser turns (sessions are
/// per-connection, created on demand).
#[derive(Clone)]
struct AppState {
    runner: Arc<Runner>,
    sessions: Arc<dyn SessionService>,
}

/// Run the web server on `port`.
pub async fn run_server(port: u16) -> anyhow::Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")
        .map_err(|_| anyhow::anyhow!("GOOGLE_API_KEY is not set"))?;

    let model: Arc<dyn Llm> = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);
    let workspace = Workspace::new(std::env::current_dir()?).allow_bash(true);
    // A deliberate mix of streaming and one-shot tools. `bash` streams via
    // emit_progress; the others return a single result. The UI renders all of
    // them from first-class events — proving tool output is no longer
    // bash-only.
    let agent = LlmAgentBuilder::new("dev-demo")
        .model(model)
        .instruction(INSTRUCTION)
        .tool(Arc::new(BashTool::new(workspace.clone())))
        .tool(Arc::new(ReadFileTool::new(workspace.clone())))
        .tool(Arc::new(GrepTool::new(workspace.clone())))
        .tool(Arc::new(GlobTool::new(workspace)))
        .build()?;
    let agent: Arc<dyn Agent> = Arc::new(agent);

    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let runner = Arc::new(
        Runner::builder()
            .app_name(APP_NAME)
            .agent(agent)
            .session_service(sessions.clone())
            .build()?,
    );

    let state = AppState { runner, sessions };

    let app = Router::new()
        .route("/", get(serve_index))
        .route("/ws", get(ws_handler))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await?;
    info!("listening on http://localhost:{port}");
    println!("\n  ▶ open http://localhost:{port} in your browser\n");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Serve the embedded single-page UI.
async fn serve_index() -> impl IntoResponse {
    Html(include_str!("../assets/index.html"))
}

/// A message the browser sends up: a user prompt for a given session.
#[derive(Debug, Deserialize)]
struct ClientMsg {
    prompt: String,
    session_id: String,
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

/// One browser connection: receive prompts, stream the agent's events back.
async fn handle_ws(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();

    while let Some(Ok(frame)) = receiver.next().await {
        let text = match frame {
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue,
        };
        let Ok(msg) = serde_json::from_str::<ClientMsg>(&text) else {
            continue;
        };

        // Ensure the session exists (idempotent).
        let _ = state
            .sessions
            .create(CreateRequest {
                app_name: APP_NAME.to_string(),
                user_id: USER_ID.to_string(),
                session_id: Some(msg.session_id.clone()),
                state: Default::default(),
            })
            .await;

        if let Err(e) = run_turn(&state.runner, &mut sender, &msg).await {
            warn!(error = %e, "turn failed");
            let _ = sender
                .send(Message::Text(
                    json!({ "type": "error", "message": e.to_string() }).to_string().into(),
                ))
                .await;
        }

        // Signal end-of-turn so the UI can re-enable input.
        if sender
            .send(Message::Text(json!({ "type": "turn_done" }).to_string().into()))
            .await
            .is_err()
        {
            break;
        }
    }
}

/// Run one agent turn, forwarding each event to the browser as it arrives.
///
/// A *single* `EventStream` carries the model's text, the tool calls, their live
/// progress, and their final results. We translate each into a typed frame using
/// first-class accessors — [`Event::tool_calls`], [`Event::tool_progress_stream`],
/// and [`Event::tool_results`] — so the browser renders any tool generically.
async fn run_turn(
    runner: &Runner,
    sender: &mut SplitSink<WebSocket, Message>,
    msg: &ClientMsg,
) -> anyhow::Result<()> {
    let content = Content::new("user").with_text(&msg.prompt);
    let mut stream =
        runner.run(UserId::new(USER_ID)?, SessionId::new(&msg.session_id)?, content).await?;

    while let Some(event) = stream.next().await {
        let event = event?;

        // 1) Tool calls → open a card per call (name + args + correlation id).
        for call in event.tool_calls() {
            let payload = json!({
                "type": "tool_call",
                "name": call.name,
                "call_id": call.call_id,
                "args": call.args,
            });
            if send(sender, payload).await.is_err() {
                return Ok(());
            }
        }

        // 2) Tool results → finalize the card with the tool's output. This is
        //    the part that makes *non-streaming* tools (read_file/grep/glob)
        //    visible, not just bash.
        for result in event.tool_results() {
            let payload = json!({
                "type": "tool_result",
                "name": result.name,
                "call_id": result.call_id,
                "response": result.response,
            });
            if send(sender, payload).await.is_err() {
                return Ok(());
            }
        }

        // 3) Live tool progress (bash stdout/stderr) → terminal lines.
        if let Some(stream_name) = event.tool_progress_stream() {
            let call_id = event
                .provider_metadata
                .get(adk_core::TOOL_PROGRESS_CALL_ID_KEY)
                .map(String::as_str);
            if let Some(content) = &event.llm_response.content {
                for part in &content.parts {
                    if let Part::Text { text } = part {
                        let payload = json!({
                            "type": "terminal",
                            "stream": stream_name, // "stdout" | "stderr"
                            "call_id": call_id,
                            "text": text,
                        });
                        if send(sender, payload).await.is_err() {
                            return Ok(());
                        }
                    }
                }
            }
            continue;
        }

        // 4) Plain model text → assistant tokens. (Tool-call/result events carry
        //    no Text parts, so this only fires for the model's own reply.)
        if let Some(content) = &event.llm_response.content {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    let payload = json!({ "type": "token", "text": text });
                    if send(sender, payload).await.is_err() {
                        return Ok(());
                    }
                }
            }
        }
    }

    Ok(())
}

/// Send one JSON frame to the browser.
async fn send(
    sender: &mut SplitSink<WebSocket, Message>,
    payload: serde_json::Value,
) -> Result<(), axum::Error> {
    sender.send(Message::Text(payload.to_string().into())).await
}
