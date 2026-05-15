//! # 09 — Quickstart: Adding Custom Tools
//!
//! Verbatim from the quickstart.md "Adding Custom Tools" section.
//! Demonstrates the `#[tool]` macro with schemars schema generation.
//!
//! ```toml
//! [dependencies]
//! adk-rust = "0.8.2"
//! adk-tool = "0.8.2"
//! schemars = "1"
//! serde = { version = "1", features = ["derive"] }
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
    Ok(json!({ "temp": 22, "city": args.city, "condition": "sunny" }))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    let agent = LlmAgentBuilder::new("weather_agent")
        .instruction("Use the get_weather tool for weather questions.")
        .model(Arc::new(model))
        .tool(Arc::new(GetWeather))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
