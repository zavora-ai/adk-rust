//! Basic FunctionTool example
//!
//! Run: cargo run --bin basic

use adk_rust::prelude::*;
use adk_rust::Launcher;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

#[derive(JsonSchema, Serialize, Deserialize)]
struct WeatherParams {
    /// The city or location to get weather for
    location: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // Weather tool with proper schema
    let weather_tool = FunctionTool::new(
        "get_weather",
        "Get current weather for a location",
        |_ctx, args| async move {
            let location = args.get("location")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            Ok(json!({
                "location": location,
                "temperature": "22°C",
                "conditions": "sunny"
            }))
        },
    )
    .with_parameters_schema::<WeatherParams>();

    let agent = LlmAgentBuilder::new("weather_agent")
        .instruction("You help users check the weather. Always use the get_weather tool when asked about weather.")
        .model(Arc::new(model))
        .tool(Arc::new(weather_tool))
        .build()?;

    println!("✅ Weather agent ready with get_weather tool");
    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}
