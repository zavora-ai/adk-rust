//! OpenAI Structured Output Example
//!
//! This example demonstrates using OpenAI with structured output schemas.
//!
//! Set OPENAI_API_KEY environment variable before running:
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! cargo run --example openai_structured --features openai
//! ```

use adk_agent::LlmAgentBuilder;
use adk_cli::Launcher;
use adk_core::Agent;
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");
    let model = OpenAIClient::new(OpenAIConfig::new(api_key, "gpt-5-mini"))?;

    // Create an agent with a defined output schema
    // This encourages the model to respond with JSON matching this structure
    let agent = LlmAgentBuilder::new("weather_agent")
        .description("A weather reporter that outputs structured JSON data")
        .model(Arc::new(model))
        .instruction(
            "You are a weather reporter. Provide weather information for the requested location. \
             Always respond with valid JSON in the following format:\n\
             {\"location\": \"city name\", \"temperature\": number, \"conditions\": \"description\", \"forecast\": [\"day1\", \"day2\", \"day3\"]}\n\
             Use fictional but realistic weather data.",
        )
        .output_schema(json!({
            "type": "object",
            "properties": {
                "location": { "type": "string", "description": "The city and state/country" },
                "temperature": { "type": "number", "description": "Temperature in Fahrenheit" },
                "conditions": { "type": "string", "description": "Short description of conditions" },
                "forecast": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "3-day forecast summary"
                }
            },
            "required": ["location", "temperature", "conditions", "forecast"],
            "additionalProperties": false
        }))
        .build()?;

    println!("OpenAI Structured Output Agent created: {}", agent.name());
    println!("This agent will respond with JSON weather data.");
    println!("Try asking: 'What is the weather in Tokyo?'\n");

    // Run with the default launcher
    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}
