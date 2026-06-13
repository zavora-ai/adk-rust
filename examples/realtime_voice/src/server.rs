//! Web server for the "Mindfulness with Mia" realtime voice app.
//!
//! Architecture — **server-side bridge** (the ADK way):
//!
//! ```text
//!   browser  ──mic PCM16 (base64 over WS)──▶  Rust /ws handler
//!   browser  ◀──assistant PCM16 + events───   IntegratedRealtimeRunner
//!                                              │
//!                                              ├─ OpenAI Realtime (gpt-realtime)  OR
//!                                              │  Gemini Live (native audio)
//!                                              ├─ SessionService     (transcripts)
//!                                              ├─ GraphMemoryService (bi-temporal KG)
//!                                              └─ weather tool        (auto-executed)
//! ```
//!
//! The Rust server owns the realtime session through
//! [`IntegratedRealtimeRunner`], so transcript persistence, memory storage, and
//! tool execution all happen server-side — exactly what the integration layer
//! exists for. The browser is a thin audio device: it streams microphone PCM up
//! and plays the PCM the server streams back.
//!
//! **Memory is a real knowledge graph.** A single file-backed
//! [`GraphMemoryService`] is shared across the process (axum state). Its compact
//! *profile card* is injected into Mia's system instruction at session start, so
//! she actually knows Shai; every completed turn is appended to the graph's
//! episodic log; and the browser's "User Memory Insights" panel reads and writes
//! the *same* graph over `/api/memory` — nothing on that panel is mocked.
//!
//! The provider is chosen per session (browser `?provider=openai|gemini`). Their
//! audio rates differ — OpenAI is 24 kHz in/out, Gemini Live is 16 kHz in /
//! 24 kHz out — so the server negotiates the rates to the browser in a `ready`
//! message before any audio flows.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::{Query, State},
    response::{Html, IntoResponse},
    routing::{get, post},
};
use base64::Engine;
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tower_http::cors::CorsLayer;
use tracing::{info, warn};

use adk_memory::{CreateEntityInput, CreateRelationInput, GraphMemoryService};
use adk_realtime::config::{RealtimeConfig, VadConfig};
use adk_realtime::events::{ServerEvent, ToolCall};
use adk_realtime::gemini::{GeminiLiveBackend, GeminiRealtimeModel};
use adk_realtime::integration::{IntegratedRealtimeRunner, IntegrationConfig};
use adk_realtime::model::BoxedModel;
use adk_realtime::openai::OpenAIRealtimeModel;
use adk_realtime::runner::FnToolHandler;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::{RelateTool, RememberTool};

use crate::tools::get_weather_tool_def;

const APP_NAME: &str = "mindfulness-mia";
const USER_ID: &str = "shai";

const MIA_INSTRUCTION: &str = "You are Mia, a calm and empathetic mindfulness coach. \
You guide users through breathing exercises, meditation, and emotional awareness. \
Speak slowly, calmly, and thoughtfully. Address the user as Shai. \
Avoid somatic grounding exercises unless explicitly requested; favor breath \
awareness and cognitive reframing. Keep responses concise and soothing. \
If the user asks about the weather, use the get_weather tool. \
When the user shares a durable fact about themselves — their name, a stable \
preference, a goal, a relationship, or a significant life event — quietly save \
it with the `remember` tool (and use `relate` to connect entities, e.g. Shai \
located_in 'Bay Area') so you recall it in future sessions. Save facts silently: \
never announce that you are saving, and do not store small talk or passing moods.";

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
            // OpenAI GA Realtime is 24 kHz both ways.
            Provider::OpenAI => (24_000, 24_000),
            // Gemini Live consumes 16 kHz PCM16 and emits 24 kHz PCM16.
            Provider::Gemini => (16_000, 24_000),
        }
    }
}

/// Shared application state. One knowledge graph, shared by the realtime bridge
/// (reads the profile card, writes episodic turns) and the `/api/memory`
/// endpoints (the Insights panel) — so the UI and the agent see the same memory.
#[derive(Clone)]
struct AppState {
    kg: Arc<GraphMemoryService>,
}

/// Run the Axum web server.
pub async fn run_server(port: u16) -> anyhow::Result<()> {
    // One process-wide, file-backed knowledge graph. Survives restarts so Mia
    // remembers Shai across sessions.
    let db = std::env::var("MIA_MEMORY_DB").unwrap_or_else(|_| "mia_memory.db".to_string());
    let kg = GraphMemoryService::new(&format!("sqlite:{db}"))
        .await
        .map_err(|e| anyhow::anyhow!("memory open failed: {e}"))?;
    kg.migrate().await.map_err(|e| anyhow::anyhow!("memory migrate failed: {e}"))?;
    seed_profile(&kg).await?;
    info!(db = %db, "knowledge-graph memory ready");

    let state = AppState { kg: Arc::new(kg) };

    let app = Router::new()
        .route("/", get(serve_index))
        .route("/ws", get(ws_handler))
        .route("/api/memory", get(get_memory).post(add_memory))
        .route("/api/memory/reset", post(reset_memory))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await?;
    info!("listening on 0.0.0.0:{port}");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Seed Shai's baseline profile into an empty graph — the facts Mia should
/// "already know" on first run. Idempotent: skipped once any entity exists.
async fn seed_profile(kg: &GraphMemoryService) -> anyhow::Result<()> {
    if kg.entity_count(APP_NAME, USER_ID).await.map_err(|e| anyhow::anyhow!("{e}"))? > 0 {
        return Ok(());
    }
    info!("seeding Shai's baseline profile into the knowledge graph");
    let entities = vec![
        CreateEntityInput {
            name: "Shai".into(),
            entity_type: "person".into(),
            observations: vec![
                "Name is spelled S-H-A-I.".into(),
                "Relocated to the Bay Area with family last week; house-hunting.".into(),
            ],
        },
        CreateEntityInput {
            name: "Bay Area".into(),
            entity_type: "place".into(),
            observations: vec!["Competitive, expensive housing market.".into()],
        },
        CreateEntityInput {
            name: "Emotional State".into(),
            entity_type: "emotion".into(),
            observations: vec![
                "Frustrated with the Bay Area housing market — paying just to apply feels insane."
                    .into(),
            ],
        },
        CreateEntityInput {
            name: "Personal Background".into(),
            entity_type: "background".into(),
            observations: vec!["Recently relocated with family; settling in.".into()],
        },
        CreateEntityInput {
            name: "Coaching Preference".into(),
            entity_type: "preference".into(),
            observations: vec![
                "Extremely important to be addressed by name.".into(),
                "Rejected somatic grounding; prefers breath awareness and cognitive reframing."
                    .into(),
            ],
        },
    ];
    kg.create_entities(APP_NAME, USER_ID, entities).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    let relations = vec![
        CreateRelationInput {
            source: "Shai".into(),
            relation_type: "located_in".into(),
            target: "Bay Area".into(),
        },
        CreateRelationInput {
            source: "Shai".into(),
            relation_type: "feels".into(),
            target: "Emotional State".into(),
        },
        CreateRelationInput {
            source: "Shai".into(),
            relation_type: "has_preference".into(),
            target: "Coaching Preference".into(),
        },
    ];
    kg.create_relations(APP_NAME, USER_ID, relations).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

/// Snapshot the user's whole graph as the JSON the Insights panel renders:
/// flattened `insights` (one per current observation, newest first) plus the
/// raw `entities` and `relations`.
async fn graph_to_json(kg: &GraphMemoryService) -> anyhow::Result<serde_json::Value> {
    let (entities, relations) =
        kg.read_graph(APP_NAME, USER_ID).await.map_err(|e| anyhow::anyhow!("{e}"))?;

    // One card per observation; the entity name is the category chip.
    let mut insights: Vec<(chrono::DateTime<chrono::Utc>, serde_json::Value)> = Vec::new();
    for e in &entities {
        for o in &e.observations {
            insights.push((
                o.valid_from,
                json!({
                    "category": e.name.to_uppercase(),
                    "content": o.content,
                    "date": o.valid_from.format("%Y-%m-%d").to_string(),
                }),
            ));
        }
    }
    insights.sort_by(|a, b| b.0.cmp(&a.0)); // newest first
    let insights: Vec<serde_json::Value> = insights.into_iter().map(|(_, v)| v).collect();

    let entities: Vec<serde_json::Value> = entities
        .iter()
        .map(|e| {
            json!({
                "name": e.name,
                "type": e.entity_type,
                "observations": e.observations.iter().map(|o| &o.content).collect::<Vec<_>>(),
            })
        })
        .collect();
    let relations: Vec<serde_json::Value> = relations
        .iter()
        .map(|r| json!({ "source": r.source, "type": r.relation_type, "target": r.target }))
        .collect();

    Ok(json!({ "insights": insights, "entities": entities, "relations": relations }))
}

/// `GET /api/memory` — the current knowledge graph for the Insights panel.
async fn get_memory(State(state): State<AppState>) -> impl IntoResponse {
    match graph_to_json(&state.kg).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => error_json(e),
    }
}

/// Body for `POST /api/memory`: attach an observation to an entity (the
/// selected category becomes the entity name).
#[derive(Debug, Deserialize)]
struct AddMemory {
    category: String,
    content: String,
}

/// `POST /api/memory` — record a new observation, then return the fresh graph.
async fn add_memory(
    State(state): State<AppState>,
    Json(body): Json<AddMemory>,
) -> impl IntoResponse {
    let content = body.content.trim().to_string();
    if content.is_empty() {
        return error_json(anyhow::anyhow!("content is empty"));
    }
    let entity = body.category.trim().to_string();
    // Append to the entity if it exists (preserves its type), else create it.
    let added =
        match state.kg.add_observations(APP_NAME, USER_ID, &entity, vec![content.clone()]).await {
            Ok(_) => Ok(()),
            Err(_) => state
                .kg
                .create_entities(
                    APP_NAME,
                    USER_ID,
                    vec![CreateEntityInput {
                        name: entity,
                        entity_type: "note".into(),
                        observations: vec![content],
                    }],
                )
                .await
                .map(|_| ()),
        };
    if let Err(e) = added {
        return error_json(anyhow::anyhow!("{e}"));
    }
    match graph_to_json(&state.kg).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => error_json(e),
    }
}

/// `POST /api/memory/reset` — wipe the user's graph and re-seed the baseline
/// profile, then return it. ("Reset to baseline", not "delete forever".)
async fn reset_memory(State(state): State<AppState>) -> impl IntoResponse {
    use adk_memory::MemoryService;
    if let Err(e) = state.kg.delete_user(APP_NAME, USER_ID).await {
        return error_json(anyhow::anyhow!("{e}"));
    }
    if let Err(e) = seed_profile(&state.kg).await {
        return error_json(e);
    }
    match graph_to_json(&state.kg).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => error_json(e),
    }
}

/// Uniform error envelope for the `/api/memory` endpoints.
fn error_json(e: anyhow::Error) -> axum::response::Response {
    warn!(error = %e, "memory endpoint failed");
    (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() })))
        .into_response()
}

/// Serve the embedded index.html.
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
    /// The user ended the session.
    #[serde(rename = "hangup")]
    Hangup,
}

/// Upgrade `/ws?provider=openai|gemini` to a per-connection realtime voice bridge.
async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<HashMap<String, String>>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let provider = params.get("provider").map(|p| Provider::parse(p)).unwrap_or(Provider::OpenAI);
    ws.on_upgrade(move |socket| handle_voice_ws(socket, provider, state.kg))
}

/// The weather tool — runs entirely server-side; its result is sent back to the
/// model automatically (auto_respond_tools) so Mia can speak it.
fn weather_tool()
-> FnToolHandler<impl Fn(&ToolCall) -> adk_realtime::error::Result<serde_json::Value> + Send + Sync>
{
    FnToolHandler::new(|call: &ToolCall| {
        let city = call.arguments.get("city").and_then(|v| v.as_str()).unwrap_or("your area");
        info!(city = %city, "🔧 weather tool executed");
        Ok(json!({
            "city": city,
            "temperature_f": 68,
            "conditions": "clear skies",
            "summary": format!("It's a calm, clear 68°F in {city}."),
        }))
    })
}

/// Build the provider-specific realtime model.
fn build_model(provider: Provider) -> anyhow::Result<(BoxedModel, &'static str)> {
    match provider {
        Provider::OpenAI => {
            let api_key = std::env::var("OPENAI_API_KEY")
                .map_err(|_| anyhow::anyhow!("OPENAI_API_KEY is not set"))?;
            let model_id = std::env::var("OPENAI_REALTIME_MODEL")
                .unwrap_or_else(|_| "gpt-realtime".to_string());
            let model: BoxedModel = Arc::new(OpenAIRealtimeModel::new(api_key, model_id));
            Ok((model, "marin")) // marin: a natural GA voice
        }
        Provider::Gemini => {
            let api_key = std::env::var("GEMINI_API_KEY")
                .or_else(|_| std::env::var("GOOGLE_API_KEY"))
                .map_err(|_| anyhow::anyhow!("GEMINI_API_KEY / GOOGLE_API_KEY is not set"))?;
            // AI Studio (API-key) endpoint uses different model names than the
            // Agent Platform/Vertex endpoint (the crate's default
            // `models/gemini-live-2.5-flash-native-audio` is the *Vertex* name and
            // 404s here). We default to the half-cascade live model, which calls
            // tools far more reliably than the native-audio model — important for
            // this tool-using agent. For the most natural voice (but weaker tool
            // use), set GEMINI_REALTIME_MODEL=models/gemini-2.5-flash-native-audio-preview-12-2025.
            let model_id = std::env::var("GEMINI_REALTIME_MODEL")
                .unwrap_or_else(|_| "models/gemini-3.1-flash-live-preview".to_string());
            let model: BoxedModel =
                Arc::new(GeminiRealtimeModel::new(GeminiLiveBackend::studio(api_key), model_id));
            Ok((model, "Kore")) // Kore: a Gemini Live voice
        }
    }
}

/// Build an [`IntegratedRealtimeRunner`] for one browser session, wiring the
/// chosen provider to an in-memory session service, the **shared** knowledge
/// graph, the weather tool, and the KG curation tools (`remember`/`relate`).
///
/// The graph's profile card is read here and baked into Mia's system
/// instruction, so she greets Shai already knowing him. (The integration layer
/// queries memory at connect but does not yet inject it into the instruction
/// itself, so we do that explicitly and leave `inject_memory_context` off.)
/// Completed turns are still appended to the graph's episodic log via
/// `store_to_memory`, and Mia can promote durable facts into structured graph
/// nodes herself via the `remember`/`relate` tools.
async fn build_runner(
    provider: Provider,
    session_id: &str,
    kg: Arc<GraphMemoryService>,
) -> anyhow::Result<IntegratedRealtimeRunner> {
    let (model, voice) = build_model(provider)?;

    // Inject what we already know about the user into the system instruction.
    let instruction = match kg.profile_card(APP_NAME, USER_ID).await {
        Ok(card) if !card.trim().is_empty() => format!("{MIA_INSTRUCTION}\n\n{card}"),
        _ => MIA_INSTRUCTION.to_string(),
    };

    // Server VAD lets the model decide turn boundaries and auto-respond — no
    // explicit create_response needed after each user utterance.
    let config = RealtimeConfig::default()
        .with_instruction(instruction)
        .with_voice(voice)
        .with_audio_only()
        .with_vad(VadConfig::server_vad())
        .with_transcription();

    let session_service = Arc::new(InMemorySessionService::new());

    // Create the session up front so transcript persistence has a home.
    session_service
        .create(CreateRequest {
            app_name: APP_NAME.to_string(),
            user_id: USER_ID.to_string(),
            session_id: Some(session_id.to_string()),
            state: Default::default(),
        })
        .await
        .map_err(|e| anyhow::anyhow!("session create failed: {e}"))?;

    // Record turns to the graph's episodic log; we inject the profile card
    // ourselves (above), so disable the integration layer's no-op injection.
    let integration_config =
        IntegrationConfig { inject_memory_context: false, ..IntegrationConfig::default() };

    let runner = IntegratedRealtimeRunner::builder()
        .model(model)
        .config(config)
        .identity(APP_NAME, USER_ID, session_id)
        .session_service(session_service)
        .memory_service(kg.clone())
        .integration_config(integration_config)
        // Self-curation: Mia can write durable facts back into the *same* graph.
        // These adk-tool built-ins are auto-bridged to the realtime ToolHandler
        // interface and scoped to (APP_NAME, USER_ID) by the integration layer —
        // so what she learns this session becomes her profile card next session.
        .adk_tool(Arc::new(RememberTool::new(kg.clone())))
        .adk_tool(Arc::new(RelateTool::new(kg)))
        .tool(get_weather_tool_def(), weather_tool())
        .build()?;

    Ok(runner)
}

/// Headless smoke test of the full integration path (no browser/mic needed).
///
/// Connects via [`IntegratedRealtimeRunner`], asks Mia a weather question by
/// text, and pumps events — reporting whether the tool executed, how much audio
/// came back, and the transcript. Run with `cargo run -- probe [openai|gemini]`.
pub async fn run_probe(provider: &str) -> anyhow::Result<()> {
    let provider = Provider::parse(provider);
    info!(provider = provider.name(), "probe: starting");
    let session_id = uuid::Uuid::new_v4().to_string();
    // Ephemeral in-memory graph for the smoke test (no persistence needed).
    let kg = GraphMemoryService::new("sqlite::memory:").await?;
    kg.migrate().await?;
    seed_profile(&kg).await?;
    let runner = build_runner(provider, &session_id, Arc::new(kg)).await?;
    runner.connect().await?;
    info!("probe: connected; sending a weather question by text");

    runner
        .send_text("What's the weather in Seattle right now? Answer in one short sentence.")
        .await?;
    runner.create_response().await?;

    let mut audio_bytes = 0usize;
    let mut transcript = String::new();
    let mut thinking = String::new();
    let mut tool_seen = false;

    loop {
        let next =
            tokio::time::timeout(std::time::Duration::from_secs(25), runner.next_event()).await;
        let event = match next {
            Ok(Some(Ok(ev))) => ev,
            Ok(Some(Err(e))) => {
                warn!(error = %e, "probe: stream error");
                break;
            }
            Ok(None) => break,
            Err(_) => {
                warn!("probe: timed out waiting for events");
                break;
            }
        };
        match event {
            ServerEvent::AudioDelta { delta, .. } => audio_bytes += delta.len(),
            // Spoken-answer transcript (OpenAI audio transcript / Gemini outputTranscription).
            ServerEvent::TranscriptDelta { delta, .. } => transcript.push_str(&delta),
            // Gemini "thinking" text (modelTurn text parts) — tracked separately.
            ServerEvent::TextDelta { delta, .. } => thinking.push_str(&delta),
            ServerEvent::FunctionCallDone { name, .. } => {
                info!(tool = %name, "probe: model requested a tool call");
                tool_seen = true;
            }
            ServerEvent::ResponseDone { .. } => {
                // A function call produces a first response that ends here; the
                // spoken answer arrives in a second response after the tool runs.
                if tool_seen && transcript.is_empty() && audio_bytes == 0 {
                    continue;
                }
                break;
            }
            ServerEvent::Error { error, .. } => {
                anyhow::bail!("realtime error: {}", error.message);
            }
            _ => {}
        }
    }

    runner.close().await.ok();
    info!(
        provider = provider.name(),
        tool_call_seen = tool_seen,
        audio_bytes,
        transcript = %transcript,
        thinking = %thinking,
        "probe: complete"
    );
    anyhow::ensure!(audio_bytes > 0 || !transcript.is_empty(), "no assistant output received");
    Ok(())
}

/// Drive one realtime voice session: pump mic audio up, stream events/audio down.
async fn handle_voice_ws(socket: WebSocket, provider: Provider, kg: Arc<GraphMemoryService>) {
    let session_id = uuid::Uuid::new_v4().to_string();
    info!(session_id = %session_id, provider = provider.name(), "voice session starting");

    let (mut sender, mut receiver) = socket.split();

    // Build + connect the integrated runner; report failures to the browser.
    let runner = match build_runner(provider, &session_id, kg).await {
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

    // Tell the browser which sample rates to use before any audio flows.
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
    info!(session_id = %session_id, provider = provider.name(), "realtime session connected");

    // Outbound: realtime events → browser. Owns the WS sink.
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
                break; // browser went away
            }
        }
    };

    // Inbound: browser mic audio → realtime session.
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
                    Ok(ClientMsg::Hangup) => break,
                    Err(_) => {} // ignore unknown control frames
                },
                Message::Close(_) => break,
                _ => {}
            }
        }
    };

    // Whichever side ends first tears down the session.
    tokio::select! {
        _ = outbound => info!(session_id = %session_id, "realtime stream ended"),
        _ = inbound => info!(session_id = %session_id, "browser disconnected"),
    }

    let _ = runner.close().await;
    info!(session_id = %session_id, "voice session closed");
}

/// Translate a realtime [`ServerEvent`] into the compact JSON the browser UI
/// consumes. Returns `None` for events the UI doesn't render.
fn server_event_to_client_json(event: ServerEvent) -> Option<serde_json::Value> {
    match event {
        ServerEvent::AudioDelta { delta, .. } => {
            // delta is decoded PCM16 bytes; re-encode for the browser to play.
            let audio = base64::engine::general_purpose::STANDARD.encode(&delta);
            Some(json!({ "type": "audio", "audio": audio }))
        }
        // The assistant's spoken words: OpenAI and Gemini both surface them as a
        // transcript delta (Gemini via outputAudioTranscription). Gemini's
        // separate `TextDelta` carries model "thinking" — intentionally not shown.
        ServerEvent::TranscriptDelta { delta, .. } => {
            Some(json!({ "type": "assistant_transcript", "delta": delta }))
        }
        // User speech transcription: OpenAI sends a single completed event;
        // Gemini streams deltas.
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
            "type": "decision",
            "text": format!("Mia is calling {name}({arguments})"),
        })),
        ServerEvent::Error { error, .. } => {
            Some(json!({ "type": "error", "message": error.message }))
        }
        _ => None,
    }
}
