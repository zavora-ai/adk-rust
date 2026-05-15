//! # 05 — Minimal with Custom Tools
//!
//! Verbatim from the README "Tool System" section.
//! Tools are included in the minimal tier — no extra features needed.
//!
//! ```toml
//! [dependencies]
//! adk-rust = "0.8.2"
//! adk-tool = "0.8.2"
//! ```

use adk_rust::Launcher;
use adk_rust::prelude::*;
use adk_tool::{AdkError, tool};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize, JsonSchema)]
struct WeatherArgs {
    /// The city to look up
    city: String,
}

/// Get the current weather for a city.
#[tool]
async fn get_weather(args: WeatherArgs) -> std::result::Result<Value, AdkError> {
    Ok(json!({ "temp": 72, "city": args.city }))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    let agent = LlmAgentBuilder::new("weather-agent")
        .instruction("You help users check the weather. Use the get_weather tool.")
        .model(Arc::new(model))
        .tool(Arc::new(GetWeather))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
