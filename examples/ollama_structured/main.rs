//! Ollama Structured Output Example
//!
//! This example demonstrates using Ollama (local models) with structured output schemas.
//!
//! Make sure Ollama is running locally before running:
//! ```bash
//! ollama serve
//! ollama pull llama3.2
//! cargo run --example ollama_structured --features ollama
//! ```

use adk_agent::LlmAgentBuilder;
use adk_cli::Launcher;
use adk_core::Agent;
use adk_model::ollama::{OllamaConfig, OllamaModel};
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Default Ollama endpoint - can be overridden with OLLAMA_HOST env var
    let host =
        std::env::var("OLLAMA_HOST").unwrap_or_else(|_| "http://localhost:11434".to_string());
    let model_name = std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "llama3.2".to_string());

    let config = OllamaConfig::with_host(&host, &model_name);
    let model = OllamaModel::new(config)?;

    // Create an agent with a defined output schema
    // This encourages the model to respond with JSON matching this structure
    let agent = LlmAgentBuilder::new("weather_agent")
        .description("A weather reporter that outputs structured JSON data")
        .model(Arc::new(model))
        .instruction(
            "You are a weather reporter. Provide weather information for the requested location. \
             Always respond with valid JSON in the following format:\n\
             {\"location\": \"city name\", \"temperature\": number, \"conditions\": \"description\", \"forecast\": [\"day1\", \"day2\", \"day3\"]}\n\
             Use fictional but realistic weather data. Only output JSON, no other text.",
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
            "required": ["location", "temperature", "conditions"]
        }))
        .build()?;

    println!("Ollama Structured Output Agent created: {}", agent.name());
    println!("Using model: {} at {}", model_name, host);
    println!("This agent will respond with JSON weather data.");
    println!("Try asking: 'What is the weather in Tokyo?'\n");

    // Run with the default launcher
    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}
