//! OpenAI Client with Local Models Example
//!
//! This example demonstrates using the OpenAI client with local models via
//! OpenAI-compatible APIs (Ollama, vLLM, LM Studio, etc.).
//!
//! Make sure Ollama is running with OpenAI compatibility:
//! ```bash
//! ollama serve
//! ollama pull llama3.2
//! cargo run --example openai_local --features openai
//! ```

use adk_agent::LlmAgentBuilder;
use adk_cli::Launcher;
use adk_core::Agent;
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Local server endpoint - Ollama exposes OpenAI-compatible API at /v1
    let base_url =
        std::env::var("LOCAL_API_URL").unwrap_or_else(|_| "http://localhost:11434/v1".to_string());
    let model_name = std::env::var("LOCAL_MODEL").unwrap_or_else(|_| "llama3.2".to_string());

    // API key can be anything for local models (Ollama ignores it)
    let api_key = std::env::var("LOCAL_API_KEY").unwrap_or_else(|_| "not-needed".to_string());

    // Use OpenAI client with compatible API endpoint
    let config = OpenAIConfig::compatible(&api_key, &base_url, &model_name);
    let model = OpenAIClient::new(config)?;

    // Create an agent with structured output schema
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

    println!("OpenAI-Compatible Local Model Agent created: {}", agent.name());
    println!("Using model: {} at {}", model_name, base_url);
    println!("This agent will respond with JSON weather data.");
    println!("Try asking: 'What is the weather in Tokyo?'\n");

    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}
