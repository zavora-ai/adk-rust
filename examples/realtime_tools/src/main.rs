//! # Realtime Tools — Function Calling in Voice Conversations (GA API)
//!
//! Demonstrates tool use with OpenAI Realtime GA API via [`IntegratedRealtimeRunner`]:
//!
//! - **Server-side tool execution** — weather, calculator, time tools auto-dispatched
//! - **Session persistence** — transcripts saved to `InMemorySessionService`
//! - **Memory storage** — completed turns stored to `InMemoryMemoryService`
//! - **Transcript aggregation** — streaming deltas assembled into complete turns
//! - **GA API protocol** — `gpt-realtime-2` with nested audio config, `output_modalities: ["audio"]`
//!
//! The `IntegratedRealtimeRunner` handles:
//! 1. Connecting to OpenAI via WebSocket
//! 2. Auto-executing tool calls when the model requests them
//! 3. Sending tool results back so the model can speak them
//! 4. Persisting each completed turn (user + assistant) to SessionService
//! 5. Storing turns to MemoryService for future retrieval
//!
//! ## Run
//!
//! ```bash
//! cargo run --manifest-path examples/realtime_tools/Cargo.toml
//! ```
//!
//! Requires: `OPENAI_API_KEY` environment variable.

use std::sync::Arc;

use adk_memory::InMemoryMemoryService;
use adk_realtime::config::{RealtimeConfig, ToolDefinition};
use adk_realtime::events::ServerEvent;
use adk_realtime::integration::{IntegratedRealtimeRunner, IntegrationConfig};
use adk_realtime::openai::OpenAIRealtimeModel;
use adk_realtime::runner::FnToolHandler;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use serde_json::json;
use tracing::info;

const APP_NAME: &str = "realtime-tools-demo";
const USER_ID: &str = "demo-user";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let api_key = std::env::var("OPENAI_API_KEY")?;
    let model_id =
        std::env::var("OPENAI_REALTIME_MODEL").unwrap_or_else(|_| "gpt-realtime-2".to_string());

    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║  Realtime Tools — Function Calling via IntegratedRunner      ║");
    println!("║                                                            ║");
    println!("║  Tools: get_weather, calculate, get_time                    ║");
    println!("║  Integration: SessionService + MemoryService                ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    // ─── Define tools ────────────────────────────────────────────────────────

    let weather_tool = ToolDefinition {
        name: "get_weather".into(),
        description: Some("Get current weather for a city".into()),
        parameters: Some(json!({
            "type": "object",
            "properties": {
                "city": { "type": "string", "description": "City name" }
            },
            "required": ["city"]
        })),
    };

    let calc_tool = ToolDefinition {
        name: "calculate".into(),
        description: Some("Evaluate a math expression".into()),
        parameters: Some(json!({
            "type": "object",
            "properties": {
                "expression": { "type": "string", "description": "Math expression like '15 + 25'" }
            },
            "required": ["expression"]
        })),
    };

    let time_tool = ToolDefinition {
        name: "get_time".into(),
        description: Some("Get current time in a timezone".into()),
        parameters: Some(json!({
            "type": "object",
            "properties": {
                "timezone": { "type": "string", "description": "Timezone like 'PST', 'JST', 'UTC'" }
            },
            "required": ["timezone"]
        })),
    };

    // ─── Build IntegratedRealtimeRunner ──────────────────────────────────────

    let session_id = uuid::Uuid::new_v4().to_string();
    let model = Arc::new(OpenAIRealtimeModel::new(&api_key, &model_id));
    let session_service = Arc::new(InMemorySessionService::new());
    let memory_service = Arc::new(InMemoryMemoryService::new());

    // Create session upfront for persistence
    session_service
        .create(CreateRequest {
            app_name: APP_NAME.to_string(),
            user_id: USER_ID.to_string(),
            session_id: Some(session_id.clone()),
            state: Default::default(),
        })
        .await?;

    let config = RealtimeConfig::default()
        .with_instruction(
            "You are a helpful assistant with weather, calculator, and time tools. \
             Use them when needed. Be concise — 2-3 sentences max.",
        )
        .with_voice("marin")
        .with_audio_only()
        // Emit the spoken answer's transcript so we can print it in the terminal.
        .with_transcription();

    let runner = IntegratedRealtimeRunner::builder()
        .model(model)
        .config(config)
        .identity(APP_NAME, USER_ID, &session_id)
        .session_service(session_service.clone())
        .memory_service(memory_service.clone())
        .integration_config(IntegrationConfig::default())
        .tool(
            weather_tool,
            FnToolHandler::new(|call| {
                let city = call.arguments["city"].as_str().unwrap_or("unknown");
                println!("  🌤️  get_weather(\"{city}\")");
                let (temp, cond) = match city.to_lowercase().as_str() {
                    "tokyo" => (75, "Partly cloudy"),
                    "london" => (58, "Overcast"),
                    "paris" => (63, "Light rain"),
                    "seattle" => (55, "Drizzle"),
                    "new york" => (68, "Clear"),
                    _ => (70, "Fair"),
                };
                Ok(json!({ "city": city, "temp_f": temp, "condition": cond }))
            }),
        )
        .tool(
            calc_tool,
            FnToolHandler::new(|call| {
                let expr = call.arguments["expression"].as_str().unwrap_or("0");
                println!("  🧮 calculate(\"{expr}\")");
                // Simple eval for demo
                let result: f64 = match expr {
                    "75 - 58" => 17.0,
                    "75 - 63" => 12.0,
                    "15 + 25" => 40.0,
                    "100 / 4" => 25.0,
                    _ => 42.0,
                };
                Ok(json!({ "expression": expr, "result": result }))
            }),
        )
        .tool(
            time_tool,
            FnToolHandler::new(|call| {
                let tz = call.arguments["timezone"].as_str().unwrap_or("UTC");
                println!("  🕐 get_time(\"{tz}\")");
                let time = match tz.to_uppercase().as_str() {
                    "PST" => "4:30 PM",
                    "JST" => "8:30 AM (+1)",
                    "GMT" | "UTC" => "12:30 AM",
                    "CET" => "1:30 AM",
                    "EST" => "7:30 PM",
                    _ => "12:00 PM",
                };
                Ok(json!({ "timezone": tz, "current_time": time }))
            }),
        )
        .build()?;

    // ─── Connect ─────────────────────────────────────────────────────────────

    println!("📡 Connecting to OpenAI Realtime ({model_id})...");
    runner.connect().await?;
    println!("✅ Connected — session {session_id}\n");
    println!("Tools registered: get_weather, calculate, get_time");
    println!("Integration: SessionService ✓  MemoryService ✓\n");

    // ─── Turn 1: Single tool call ────────────────────────────────────────────

    let q1 = "What's the weather in Tokyo right now?";
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  Turn 1: Single Tool Call");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    println!("👤 {q1}\n");

    runner.send_text(q1).await?;
    // A text input item does not auto-trigger a response (server VAD only does
    // that for *audio* turns), so ask the model to respond explicitly.
    runner.create_response().await?;
    let r1 = collect_response(&runner).await;
    println!("🤖 {r1}\n");

    // ─── Turn 2: Multi-tool (weather + time) ─────────────────────────────────

    let q2 = "What's the weather in London and what time is it there?";
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  Turn 2: Multi-Tool Call");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    println!("👤 {q2}\n");

    runner.send_text(q2).await?;
    runner.create_response().await?;
    let r2 = collect_response(&runner).await;
    println!("🤖 {r2}\n");

    // ─── Turn 3: Calculator ──────────────────────────────────────────────────

    let q3 = "What's 15 + 25?";
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  Turn 3: Calculator Tool");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    println!("👤 {q3}\n");

    runner.send_text(q3).await?;
    runner.create_response().await?;
    let r3 = collect_response(&runner).await;
    println!("🤖 {r3}\n");

    // ─── Close ───────────────────────────────────────────────────────────────

    runner.close().await?;

    // ─── Report ──────────────────────────────────────────────────────────────

    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  Summary");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    println!("✅ Session closed");
    println!();
    println!("Features demonstrated:");
    println!("  • IntegratedRealtimeRunner with ADK services");
    println!("  • FnToolHandler — closure-based tool execution");
    println!("  • Auto-dispatch: model calls tool → server executes → result sent back");
    println!("  • Multiple tools called in a single turn");
    println!("  • SessionService — transcripts persisted per turn");
    println!("  • MemoryService — turns stored for future retrieval");
    println!("  • TranscriptAggregator — streaming deltas → complete turns");
    println!("  • GA API: gpt-realtime-2, nested audio config, output_modalities: [\"audio\"]");

    Ok(())
}

/// Collect the model's spoken answer for one turn, transparently handling tools.
///
/// A tool-using turn spans more than one response: the model may emit a short
/// preamble *and* its tool call(s) in one response; the runner then auto-executes
/// the tools, sends the results back, and the model speaks its real answer in a
/// **follow-up** response. The reliable end-of-turn signal is therefore a
/// `ResponseDone` for a response that did **not** call a tool — so we keep
/// pumping across every tool-dispatch response (even several chained ones) and
/// stop only once the model finishes a response without requesting a tool.
///
/// Returning before the turn is truly finished is what triggers OpenAI's
/// "active response already in progress" error on the next turn — so getting
/// this boundary right matters.
async fn collect_response(runner: &IntegratedRealtimeRunner) -> String {
    let mut text = String::new();
    let mut audio_bytes = 0usize;
    let mut tool_in_response = false;

    loop {
        let next =
            tokio::time::timeout(std::time::Duration::from_secs(30), runner.next_event()).await;

        let event = match next {
            Ok(Some(Ok(ev))) => ev,
            Ok(Some(Err(e))) => return format!("Stream error: {e}"),
            Ok(None) => break,
            Err(_) => {
                return if text.is_empty() {
                    "(timed out waiting for response)".to_string()
                } else {
                    text
                };
            }
        };

        match event {
            // Spoken-answer transcript (audio-only output ⇒ TranscriptDelta).
            ServerEvent::TextDelta { delta, .. } | ServerEvent::TranscriptDelta { delta, .. } => {
                text.push_str(&delta);
            }
            ServerEvent::AudioDelta { delta, .. } => audio_bytes += delta.len(),
            ServerEvent::FunctionCallDone { name, .. } => {
                info!("  tool call completed: {name}");
                tool_in_response = true;
            }
            ServerEvent::ResponseDone { .. } => {
                // A response that dispatched tool call(s) is followed by another
                // response (once the runner returns the results) where the model
                // actually speaks. Only a response that ends WITHOUT a tool call
                // completes the turn.
                if tool_in_response {
                    tool_in_response = false;
                    continue;
                }
                break;
            }
            ServerEvent::Error { error, .. } => {
                return format!("Error: {}", error.message);
            }
            _ => {}
        }
    }

    if !text.is_empty() {
        text
    } else if audio_bytes > 0 {
        "(audio-only response — enable .with_transcription() to see text)".to_string()
    } else {
        "(no response)".to_string()
    }
}
