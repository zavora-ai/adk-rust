//! OpenAI Realtime — Dynamic Session Update Example
//!
//! Demonstrates mid-session context mutation using OpenAI's `session.update` API.
//! The agent starts as a general assistant, then switches to a travel agent
//! persona without dropping the connection.
//!
//! Requires: `OPENAI_API_KEY` environment variable.
//!
//! Run: `cargo run -p adk-realtime --features openai --example openai_session_update`

use adk_realtime::config::{RealtimeConfig, SessionUpdateConfig, ToolDefinition};
use adk_realtime::events::ServerEvent;
use adk_realtime::runner::{FnToolHandler, RealtimeRunner};
use serde_json::json;
use tracing::{error, info};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    dotenvy::dotenv().ok();

    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");
    let model_id =
        std::env::var("OPENAI_REALTIME_MODEL").unwrap_or("gpt-4o-realtime-preview".into());

    info!("connecting to OpenAI Realtime with model {model_id}");

    // Phase 1: General assistant
    let model = adk_realtime::openai::OpenAIRealtimeModel::new(&api_key, &model_id);

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

    let runner = RealtimeRunner::builder()
        .model(std::sync::Arc::new(model))
        .config(
            RealtimeConfig::default()
                .with_instruction("You are a helpful general assistant. Be concise.")
                .with_voice("alloy"),
        )
        .tool(
            weather_tool,
            FnToolHandler::new(|call| {
                let city = call.arguments["city"].as_str().unwrap_or("unknown").to_string();
                info!("weather tool called for {city}");
                Ok(json!({"city": city, "temp_f": 72, "condition": "sunny"}))
            }),
        )
        .build()?;

    runner.connect().await?;
    info!("connected — phase 1: general assistant");

    // Send a text message
    runner.send_text("What's the weather in Seattle?").await?;
    runner.create_response().await?;

    // Process events until response is done
    let mut response_text = String::new();
    while let Some(event) = runner.next_event().await {
        match event {
            Ok(ServerEvent::TextDelta { delta, .. }) => {
                response_text.push_str(&delta);
            }
            Ok(ServerEvent::ResponseDone { .. }) => {
                info!("phase 1 response: {response_text}");
                break;
            }
            Ok(ServerEvent::FunctionCallDone { .. }) => {
                // Tool execution handled by runner
            }
            Ok(ServerEvent::Error { error, .. }) => {
                error!("error: {}", error.message);
                break;
            }
            Err(e) => {
                error!("stream error: {e}");
                break;
            }
            _ => {}
        }
    }

    // Phase 2: Switch to travel agent persona mid-session
    info!("switching to travel agent persona...");

    let travel_tools = vec![ToolDefinition {
        name: "search_flights".into(),
        description: Some("Search for flights between cities".into()),
        parameters: Some(json!({
            "type": "object",
            "properties": {
                "from": { "type": "string" },
                "to": { "type": "string" },
                "date": { "type": "string" }
            },
            "required": ["from", "to"]
        })),
    }];

    let update = SessionUpdateConfig(
        RealtimeConfig::default()
            .with_instruction(
                "You are now a travel agent. Help users find flights and plan trips. Be enthusiastic.",
            )
            .with_tools(travel_tools),
    );

    runner.update_session(update).await?;
    info!("session updated — phase 2: travel agent");

    // Send a travel query
    runner.send_text("I need a flight from Seattle to Tokyo next month").await?;
    runner.create_response().await?;

    response_text.clear();
    while let Some(event) = runner.next_event().await {
        match event {
            Ok(ServerEvent::TextDelta { delta, .. }) => {
                response_text.push_str(&delta);
            }
            Ok(ServerEvent::ResponseDone { .. }) => {
                info!("phase 2 response: {response_text}");
                break;
            }
            Ok(ServerEvent::Error { error, .. }) => {
                error!("error: {}", error.message);
                break;
            }
            Err(e) => {
                error!("stream error: {e}");
                break;
            }
            _ => {}
        }
    }

    runner.close().await?;
    info!("session closed");

    Ok(())
}
