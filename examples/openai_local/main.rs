//! OpenAI Client with Local Models Example
//!
//! This example demonstrates using the OpenAI client with local models via
//! OpenAI-compatible APIs (Ollama, vLLM, LM Studio, etc.).
//!
//! Make sure Ollama is running with OpenAI compatibility:
//! ```bash
//! ollama serve
//! ollama pull qwen3.5
//! cargo run --example openai_local --features openai
//! ```

use adk_agent::LlmAgentBuilder;
use adk_cli::Launcher;
use adk_core::{Agent, Result, ToolContext};
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use adk_tool::FunctionTool;
use serde_json::{Value, json};
use std::sync::Arc;

/// Get weather for Tokyo - returns mock weather data
async fn get_tokyo_weather(_ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
    Ok(json!({
        "location": "Tokyo, Japan",
        "temperature": 72,
        "conditions": "Partly cloudy with light breeze",
        "humidity": 65,
        "forecast": [
            "Tomorrow: Sunny, 75°F",
            "Day 2: Cloudy, 68°F",
            "Day 3: Rain expected, 62°F"
        ]
    }))
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Local server endpoint - Ollama exposes OpenAI-compatible API at /v1
    let base_url =
        std::env::var("LOCAL_API_URL").unwrap_or_else(|_| "http://localhost:11434/v1".to_string());
    let model_name = std::env::var("LOCAL_MODEL").unwrap_or_else(|_| "qwen3.5".to_string());
    let structured_output = std::env::var("LOCAL_STRUCTURED_OUTPUT")
        .ok()
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false);

    // API key can be anything for local models (Ollama ignores it)
    let api_key = std::env::var("LOCAL_API_KEY").unwrap_or_else(|_| "not-needed".to_string());

    // Use OpenAI client with compatible API endpoint
    let config = OpenAIConfig::compatible(&api_key, &base_url, &model_name);
    let model = OpenAIClient::new(config)?;

    // Create the weather tool
    let weather_tool = FunctionTool::new(
        "get_tokyo_weather",
        "Get current weather information for Tokyo, Japan. Returns temperature, conditions, humidity, and 3-day forecast.",
        get_tokyo_weather,
    );

    // Default to plain tool-augmented responses for maximum compatibility with
    // local OpenAI-compatible servers. Strict JSON schema output can be enabled
    // with LOCAL_STRUCTURED_OUTPUT=1 for servers/models that support it.
    let mut agent = LlmAgentBuilder::new("weather_agent")
        .description("A weather assistant that can get Tokyo weather")
        .model(Arc::new(model))
        .instruction(
            "You are a weather assistant. When asked about weather in Tokyo, \
             use the get_tokyo_weather tool to fetch the data. \
             If structured output is not enabled, respond clearly in plain text.",
        )
        .tool(Arc::new(weather_tool));

    if structured_output {
        agent = agent.output_schema(json!({
            "type": "object",
            "properties": {
                "location": { "type": "string" },
                "temperature": { "type": "number" },
                "conditions": { "type": "string" },
                "humidity": { "type": "number" },
                "forecast": {
                    "type": "array",
                    "items": { "type": "string" }
                }
            },
            "required": ["location", "temperature", "conditions", "humidity", "forecast"],
            "additionalProperties": false
        }));
    }

    let agent = agent.build()?;

    println!("OpenAI-Compatible Local Model Agent created: {}", agent.name());
    println!("Using model: {} at {}", model_name, base_url);
    println!("This agent has a tool to get Tokyo weather.");
    println!("Structured output: {}", if structured_output { "enabled" } else { "disabled" });
    println!("Try asking: 'What is the weather in Tokyo?'\n");

    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}
