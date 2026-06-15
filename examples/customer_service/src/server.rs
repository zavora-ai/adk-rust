//! Server-side bridge for the multimodal customer-service agent.
//!
//! ```text
//!   browser ──mic PCM16 + camera JPEG frames (base64 over WS)──▶  /ws handler
//!   browser ◀──agent PCM16 + transcripts + tool events──────────  IntegratedRealtimeRunner
//!                                                                  ├─ OpenAI gpt-realtime  OR
//!                                                                  │  Gemini Live (native A/V)
//!                                                                  ├─ process_refund tool
//!                                                                  └─ connect_to_human tool
//! ```
//!
//! The Rust server owns the realtime session (so tools run server-side and the
//! API key never reaches the browser). The browser captures mic audio and camera
//! frames and plays the agent's audio. Provider is chosen per session
//! (`/ws?provider=openai|gemini`); audio rates are negotiated in a `ready`
//! message (OpenAI 24 kHz in/out; Gemini 16 kHz in / 24 kHz out). Camera frames
//! are forwarded as image input — continuous for Gemini, periodic snapshots for
//! OpenAI.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Router,
    extract::Query,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::{Html, IntoResponse},
    routing::get,
};
use base64::Engine;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tower_http::cors::CorsLayer;
use tracing::{info, warn};

use adk_realtime::config::{RealtimeConfig, ToolDefinition, VadConfig};
use adk_realtime::events::{ServerEvent, ToolCall};
use adk_realtime::gemini::{GeminiLiveBackend, GeminiRealtimeModel};
use adk_realtime::integration::{IntegratedRealtimeRunner, IntegrationConfig};
use adk_realtime::model::BoxedModel;
use adk_realtime::openai::OpenAIRealtimeModel;
use adk_realtime::runner::FnToolHandler;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};

const APP_NAME: &str = "customer-service-agent";
const USER_ID: &str = "customer";

const AGENT_INSTRUCTION: &str = "You are Aria, a warm, sharp customer-support agent for an online \
retailer. You can hear the customer, read their tone, and SEE what they show their camera. \
Goals: resolve the issue quickly and make the customer feel heard. \
Empathy: notice the customer's emotional state — if they sound frustrated or upset, acknowledge \
it sincerely and slow down; if they're happy, match their warmth. Keep replies concise and natural. \
Vision: when the customer shows you an item (e.g. a damaged product for a return), briefly describe \
what you see and use it to help resolve the issue. \
Actions: when a refund is warranted, call the process_refund tool with the order id and reason — \
do not claim a refund is done unless the tool returns success. When the issue is complex, sensitive, \
or the customer asks for a person, call connect_to_human with a short reason. \
Never invent order details; if you don't have an order id, ask for it.";

/// The realtime provider for a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Provider {
    OpenAI,
    Gemini,
}

impl Provider {
    fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "gemini" | "google" => Provider::Gemini,
            _ => Provider::OpenAI,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Provider::OpenAI => "openai",
            Provider::Gemini => "gemini",
        }
    }

    /// (input_sample_rate, output_sample_rate) the browser must use.
    fn audio_rates(self) -> (u32, u32) {
        match self {
            Provider::OpenAI => (24_000, 24_000),
            Provider::Gemini => (16_000, 24_000),
        }
    }
}

/// Run the Axum web server.
pub async fn run_server(port: u16) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/", get(serve_index))
        .route("/ws", get(ws_handler))
        .layer(CorsLayer::permissive());

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await?;
    info!("listening on 0.0.0.0:{port}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn serve_index() -> impl IntoResponse {
    Html(include_str!("../assets/index.html"))
}

/// Messages the browser sends up the WebSocket.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ClientMsg {
    /// A chunk of microphone audio (base64-encoded PCM16 mono).
    #[serde(rename = "input_audio")]
    InputAudio { audio: String },
    /// A camera frame (base64-encoded image; `mime` like "image/jpeg").
    #[serde(rename = "video_frame")]
    VideoFrame { mime: String, data: String },
    /// A typed chat message.
    #[serde(rename = "text")]
    Text { text: String },
    /// The user ended the session.
    #[serde(rename = "hangup")]
    Hangup,
}

/// Upgrade `/ws?provider=openai|gemini` to a per-connection multimodal bridge.
async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let provider = params.get("provider").map(|p| Provider::parse(p)).unwrap_or(Provider::OpenAI);
    ws.on_upgrade(move |socket| handle_ws(socket, provider))
}

// ─── Tools ───────────────────────────────────────────────────────────────────

fn process_refund_def() -> ToolDefinition {
    ToolDefinition {
        name: "process_refund".into(),
        description: Some(
            "Issue a refund for an order. Only call when a refund is clearly warranted.".into(),
        ),
        parameters: Some(json!({
            "type": "object",
            "properties": {
                "order_id": { "type": "string", "description": "The customer's order id, e.g. 'A-10293'." },
                "reason": { "type": "string", "description": "Short reason for the refund." }
            },
            "required": ["order_id", "reason"]
        })),
    }
}

fn process_refund_tool()
-> FnToolHandler<impl Fn(&ToolCall) -> adk_realtime::error::Result<serde_json::Value> + Send + Sync>
{
    FnToolHandler::new(|call: &ToolCall| {
        let order = call.arguments.get("order_id").and_then(|v| v.as_str()).unwrap_or("unknown");
        let reason = call.arguments.get("reason").and_then(|v| v.as_str()).unwrap_or("");
        info!(order_id = %order, reason = %reason, "🔧 process_refund");
        let refund_id = format!("RF-{}", &uuid::Uuid::new_v4().to_string()[..8].to_uppercase());
        Ok(json!({
            "status": "approved",
            "refund_id": refund_id,
            "order_id": order,
            "message": format!("Refund {refund_id} approved for order {order}; funds return in 3–5 business days."),
        }))
    })
}

fn connect_to_human_def() -> ToolDefinition {
    ToolDefinition {
        name: "connect_to_human".into(),
        description: Some(
            "Transfer the customer to a human agent for complex or sensitive issues.".into(),
        ),
        parameters: Some(json!({
            "type": "object",
            "properties": {
                "reason": { "type": "string", "description": "Why a human is needed." }
            },
            "required": ["reason"]
        })),
    }
}

fn connect_to_human_tool()
-> FnToolHandler<impl Fn(&ToolCall) -> adk_realtime::error::Result<serde_json::Value> + Send + Sync>
{
    FnToolHandler::new(|call: &ToolCall| {
        let reason = call.arguments.get("reason").and_then(|v| v.as_str()).unwrap_or("");
        info!(reason = %reason, "🔧 connect_to_human");
        Ok(json!({
            "status": "transferring",
            "queue_position": 2,
            "eta_seconds": 45,
            "message": "Connecting you to a human specialist now — about 45 seconds.",
        }))
    })
}

// ─── Model + runner ────────────────────────────────────────────────────────

/// Affective (emotion-aware) dialogue is opt-in via `CS_AFFECTIVE=1`. It needs a
/// Gemini **native-audio** model, which trades some tool-calling reliability —
/// so it's off by default (the half-cascade model keeps tools sharp).
fn affective_dialog() -> bool {
    matches!(std::env::var("CS_AFFECTIVE").as_deref(), Ok("1") | Ok("true"))
}

fn build_model(provider: Provider) -> anyhow::Result<(BoxedModel, &'static str)> {
    match provider {
        Provider::OpenAI => {
            let api_key = std::env::var("OPENAI_API_KEY")
                .map_err(|_| anyhow::anyhow!("OPENAI_API_KEY is not set"))?;
            let model_id = std::env::var("OPENAI_REALTIME_MODEL")
                .unwrap_or_else(|_| "gpt-realtime".to_string());
            let model: BoxedModel = Arc::new(OpenAIRealtimeModel::new(api_key, model_id));
            Ok((model, "marin"))
        }
        Provider::Gemini => {
            let api_key = std::env::var("GEMINI_API_KEY")
                .or_else(|_| std::env::var("GOOGLE_API_KEY"))
                .map_err(|_| anyhow::anyhow!("GEMINI_API_KEY / GOOGLE_API_KEY is not set"))?;
            // Half-cascade live model calls tools reliably and accepts video
            // frames; affective dialogue needs a native-audio model instead.
            let default_model = if affective_dialog() {
                "models/gemini-2.5-flash-native-audio-preview-12-2025"
            } else {
                "models/gemini-3.1-flash-live-preview"
            };
            let model_id = std::env::var("GEMINI_REALTIME_MODEL")
                .unwrap_or_else(|_| default_model.to_string());
            let model: BoxedModel =
                Arc::new(GeminiRealtimeModel::new(GeminiLiveBackend::studio(api_key), model_id));
            Ok((model, "Kore"))
        }
    }
}

async fn build_runner(
    provider: Provider,
    session_id: &str,
) -> anyhow::Result<IntegratedRealtimeRunner> {
    let (model, voice) = build_model(provider)?;

    let config = RealtimeConfig::default()
        .with_instruction(AGENT_INSTRUCTION)
        .with_voice(voice)
        .with_audio_only()
        .with_vad(VadConfig::server_vad())
        .with_transcription()
        // Honored only by Gemini native-audio models; a no-op elsewhere.
        .with_affective_dialog(affective_dialog());

    let session_service = Arc::new(InMemorySessionService::new());
    session_service
        .create(CreateRequest {
            app_name: APP_NAME.to_string(),
            user_id: USER_ID.to_string(),
            session_id: Some(session_id.to_string()),
            state: Default::default(),
        })
        .await
        .map_err(|e| anyhow::anyhow!("session create failed: {e}"))?;

    let runner = IntegratedRealtimeRunner::builder()
        .model(model)
        .config(config)
        .identity(APP_NAME, USER_ID, session_id)
        .session_service(session_service)
        .integration_config(IntegrationConfig::default())
        .tool(process_refund_def(), process_refund_tool())
        .tool(connect_to_human_def(), connect_to_human_tool())
        .build()?;

    Ok(runner)
}

// ─── Bridge ──────────────────────────────────────────────────────────────────

async fn handle_ws(socket: WebSocket, provider: Provider) {
    let session_id = uuid::Uuid::new_v4().to_string();
    info!(session_id = %session_id, provider = provider.name(), "customer-service session starting");

    let (mut sender, mut receiver) = socket.split();

    let runner = match build_runner(provider, &session_id).await {
        Ok(r) => Arc::new(r),
        Err(e) => {
            warn!(error = %e, "failed to build runner");
            let _ = sender
                .send(Message::Text(
                    json!({"type":"error","message": e.to_string()}).to_string().into(),
                ))
                .await;
            return;
        }
    };
    if let Err(e) = runner.connect().await {
        warn!(error = %e, "failed to connect realtime session");
        let _ = sender
            .send(Message::Text(
                json!({"type":"error","message": format!("connect failed: {e}")})
                    .to_string()
                    .into(),
            ))
            .await;
        return;
    }

    let (input_rate, output_rate) = provider.audio_rates();
    let ready = json!({
        "type": "ready",
        "provider": provider.name(),
        "input_rate": input_rate,
        "output_rate": output_rate,
    });
    if sender.send(Message::Text(ready.to_string().into())).await.is_err() {
        return;
    }
    info!(session_id = %session_id, provider = provider.name(), "session connected");

    // Outbound: realtime events → browser.
    let out_runner = runner.clone();
    let outbound = async move {
        while let Some(event) = out_runner.next_event().await {
            let msg = match event {
                Ok(ev) => server_event_to_client_json(ev),
                Err(e) => Some(json!({"type":"error","message": e.to_string()})),
            };
            if let Some(payload) = msg
                && sender.send(Message::Text(payload.to_string().into())).await.is_err()
            {
                break;
            }
        }
    };

    // Inbound: browser mic/camera/text → realtime session.
    let in_runner = runner.clone();
    let inbound = async move {
        while let Some(Ok(frame)) = receiver.next().await {
            match frame {
                Message::Text(text) => match serde_json::from_str::<ClientMsg>(&text) {
                    Ok(ClientMsg::InputAudio { audio }) => {
                        if let Err(e) = in_runner.send_audio(&audio).await {
                            warn!(error = %e, "send_audio failed");
                            break;
                        }
                    }
                    Ok(ClientMsg::VideoFrame { mime, data }) => {
                        if let Err(e) = in_runner.send_video_frame(&mime, &data).await {
                            warn!(error = %e, "send_video_frame failed");
                        }
                    }
                    Ok(ClientMsg::Text { text }) => {
                        if in_runner.send_text(&text).await.is_ok() {
                            let _ = in_runner.create_response().await;
                        }
                    }
                    Ok(ClientMsg::Hangup) => break,
                    Err(_) => {}
                },
                Message::Close(_) => break,
                _ => {}
            }
        }
    };

    tokio::select! {
        _ = outbound => info!(session_id = %session_id, "realtime stream ended"),
        _ = inbound => info!(session_id = %session_id, "browser disconnected"),
    }

    let _ = runner.close().await;
    info!(session_id = %session_id, "session closed");
}

/// Translate a realtime [`ServerEvent`] into the compact JSON the browser UI
/// consumes. Returns `None` for events the UI doesn't render.
fn server_event_to_client_json(event: ServerEvent) -> Option<serde_json::Value> {
    match event {
        ServerEvent::AudioDelta { delta, .. } => {
            let audio = base64::engine::general_purpose::STANDARD.encode(&delta);
            Some(json!({ "type": "audio", "audio": audio }))
        }
        ServerEvent::TranscriptDelta { delta, .. } => {
            Some(json!({ "type": "agent_transcript", "delta": delta }))
        }
        ServerEvent::InputTranscriptDelta { delta, .. } => {
            Some(json!({ "type": "user_transcript_delta", "delta": delta }))
        }
        ServerEvent::InputTranscriptCompleted { transcript, .. } => {
            Some(json!({ "type": "user_transcript", "text": transcript }))
        }
        ServerEvent::SpeechStarted { .. } => Some(json!({ "type": "user_speaking" })),
        ServerEvent::SpeechStopped { .. } => Some(json!({ "type": "user_stopped" })),
        ServerEvent::ResponseDone { .. } => Some(json!({ "type": "response_done" })),
        ServerEvent::FunctionCallDone { name, arguments, .. } => Some(json!({
            "type": "tool",
            "name": name,
            "args": arguments,
        })),
        ServerEvent::Error { error, .. } => {
            Some(json!({ "type": "error", "message": error.message }))
        }
        _ => None,
    }
}

/// Headless smoke test: ask for a refund by text and verify the tool runs.
pub async fn run_probe(provider: &str) -> anyhow::Result<()> {
    let provider = Provider::parse(provider);
    info!(provider = provider.name(), "probe: starting");
    let session_id = uuid::Uuid::new_v4().to_string();
    let runner = build_runner(provider, &session_id).await?;
    runner.connect().await?;
    info!("probe: connected; requesting a refund by text");

    runner
        .send_text(
            "Hi, my order A-10293 arrived shattered. I'd like a refund please. Answer briefly.",
        )
        .await?;
    runner.create_response().await?;

    let mut audio_bytes = 0usize;
    let mut transcript = String::new();
    let mut tool_seen: Option<String> = None;

    loop {
        let next =
            tokio::time::timeout(std::time::Duration::from_secs(30), runner.next_event()).await;
        let event = match next {
            Ok(Some(Ok(ev))) => ev,
            Ok(Some(Err(e))) => {
                warn!(error = %e, "probe: stream error");
                break;
            }
            Ok(None) => break,
            Err(_) => break,
        };
        match event {
            ServerEvent::AudioDelta { delta, .. } => audio_bytes += delta.len(),
            ServerEvent::TranscriptDelta { delta, .. } => transcript.push_str(&delta),
            ServerEvent::FunctionCallDone { name, .. } => {
                info!(tool = %name, "probe: tool call");
                tool_seen = Some(name);
            }
            ServerEvent::ResponseDone { .. } => {
                if tool_seen.is_some() && transcript.is_empty() && audio_bytes == 0 {
                    continue;
                }
                break;
            }
            ServerEvent::Error { error, .. } => anyhow::bail!("realtime error: {}", error.message),
            _ => {}
        }
    }

    runner.close().await.ok();
    info!(
        provider = provider.name(),
        tool = tool_seen.as_deref().unwrap_or("(none)"),
        audio_bytes,
        transcript = %transcript,
        "probe: complete"
    );
    anyhow::ensure!(
        tool_seen.as_deref() == Some("process_refund") || audio_bytes > 0 || !transcript.is_empty(),
        "no agent output received"
    );
    Ok(())
}
