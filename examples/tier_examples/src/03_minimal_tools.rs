//! # 03 — Minimal with Custom Tools
//!
//! Tools are included in the minimal tier — no extra features needed.
//! Uses the #[tool] macro for zero-boilerplate tool definitions.
//!
//! ```toml
//! [dependencies]
//! adk-rust = "0.8.0"
//! ```

use adk_rust::prelude::*;
use adk_rust::Launcher;
use adk_tool::{tool, AdkError};
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
async fn get_weather(args: WeatherArgs) -> Result<Value, AdkError> {
    Ok(json!({
        "city": args.city,
        "temperature": "22°C",
        "condition": "Sunny"
    }))
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
