//! # OrcaRouter Example
//!
//! Runs an `LlmAgent` against OrcaRouter through the
//! `OpenAICompatibleConfig::orcarouter` preset:
//!
//! 1. **Chat** — a single question and answer
//! 2. **Tool calling** — the agent calls a local function tool
//! 3. **Second vendor** — the same key and client with an `anthropic/` model ID
//!
//! ## Run
//!
//! ```bash
//! cd examples/orcarouter
//! cp .env.example .env   # add your ORCAROUTER_API_KEY
//! cargo run
//! ```

use std::io::Write;
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Content, EventTextDeltas, Llm, Part};
use adk_model::{OpenAICompatible, OpenAICompatibleConfig};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::{AdkError, tool};
use futures::StreamExt;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use tracing_subscriber::EnvFilter;

const APP_NAME: &str = "orcarouter-example";
const DEFAULT_MODEL: &str = "openai/gpt-5.6-terra";
const SECOND_VENDOR_MODEL: &str = "anthropic/claude-sonnet-5";

#[derive(Deserialize, JsonSchema)]
struct WeatherArgs {
    /// City name, for example "Nairobi".
    city: String,
}

/// Return the current weather for a city.
#[tool]
async fn get_weather(args: WeatherArgs) -> Result<Value, AdkError> {
    // Fixed readings keep the example deterministic and free of a second API key.
    let (condition, celsius) = match args.city.to_lowercase().as_str() {
        "nairobi" => ("partly cloudy", 22),
        "london" => ("light rain", 14),
        "tokyo" => ("clear", 19),
        _ => ("no reading", 0),
    };
    Ok(json!({ "city": args.city, "condition": condition, "temperature_c": celsius }))
}

/// Builds a single-agent runner over `model`, sends `prompt`, and prints the reply as it streams.
async fn ask(
    model: Arc<dyn Llm>,
    tools: bool,
    session_id: &str,
    prompt: &str,
) -> anyhow::Result<()> {
    let mut builder = LlmAgentBuilder::new("orcarouter_agent")
        .description("Agent served through OrcaRouter")
        .instruction("You are a concise assistant. Answer in at most two sentences.")
        .model(model);
    if tools {
        builder = builder.tool(Arc::new(GetWeather));
    }
    let agent: Arc<dyn Agent> = Arc::new(builder.build()?);

    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP_NAME.into(),
            user_id: "user".into(),
            session_id: Some(session_id.into()),
            state: Default::default(),
        })
        .await?;
    let runner =
        Runner::builder().app_name(APP_NAME).agent(agent).session_service(sessions).build()?;

    println!("  👤 {prompt}");
    print!("  🤖 ");
    let mut stream =
        runner.run_str("user", session_id, Content::new("user").with_text(prompt)).await?;
    // Streams can end with a complete snapshot; the adapter turns it back into deltas.
    let mut deltas = EventTextDeltas::default();
    while let Some(event) = stream.next().await {
        let event = event?;
        let event = deltas.push(&event);
        let Some(content) = event.content() else { continue };
        for part in &content.parts {
            match part {
                Part::Text { text } => print!("{text}"),
                Part::FunctionCall { name, args, .. } => print!("\n  🔧 {name}({args})\n  🤖 "),
                Part::FunctionResponse { function_response, .. } => {
                    print!("\n  ↩ {}\n  🤖 ", function_response.response);
                }
                _ => {}
            }
        }
        std::io::stdout().flush()?;
    }
    println!("\n");
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .init();

    let api_key = std::env::var("ORCAROUTER_API_KEY")
        .map_err(|_| anyhow::anyhow!("ORCAROUTER_API_KEY must be set — see .env.example"))?;
    let model_id = std::env::var("ORCAROUTER_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.into());

    println!("╔══════════════════════════════════════════════╗");
    println!("║  OrcaRouter Example — ADK-Rust               ║");
    println!("╚══════════════════════════════════════════════╝\n");

    let model: Arc<dyn Llm> =
        Arc::new(OpenAICompatible::new(OpenAICompatibleConfig::orcarouter(&api_key, &model_id))?);

    println!("── 1. Chat ({model_id}) ──");
    ask(model.clone(), false, "chat", "What does an OpenAI-compatible API endpoint mean?").await?;

    println!("── 2. Tool calling ({model_id}) ──");
    ask(model, true, "tools", "What is the weather in Nairobi right now?").await?;

    println!("── 3. Second vendor ({SECOND_VENDOR_MODEL}) ──");
    let second: Arc<dyn Llm> = Arc::new(OpenAICompatible::new(
        OpenAICompatibleConfig::orcarouter(&api_key, SECOND_VENDOR_MODEL),
    )?);
    ask(second, false, "second-vendor", "Name one practical use of a model router.").await?;

    Ok(())
}
