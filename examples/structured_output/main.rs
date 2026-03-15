use adk_agent::LlmAgentBuilder;
use adk_cli::Launcher;
use adk_core::Agent;
use adk_model::GeminiModel;
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Load environment variables
    dotenvy::dotenv().ok();

    let api_key = std::env::var("GOOGLE_API_KEY").expect("GOOGLE_API_KEY must be set");
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // Create an agent with a defined output schema
    // This forces the model to respond with JSON matching this structure
    let agent = LlmAgentBuilder::new("weather_agent")
        .description("A weather reporter that outputs structured JSON data")
        .model(Arc::new(model))
        .instruction("You are a weather reporter. Provide weather information for the requested location in the specified JSON format.")
        .output_schema(json!({
            "type": "object",
            "properties": {
                "location": { "type": "string", "description": "The city and state/country" },
                "temperature": { "type": "number", "description": "Temperature in Fahrenheit" },
                "conditions": { "type": "string", "description": "Short description of conditions (e.g. Sunny, Cloudy)" },
                "forecast": { 
                    "type": "array", 
                    "items": { "type": "string" },
                    "description": "3-day forecast summary"
                }
            },
            "required": ["location", "temperature", "conditions"]
        }))
        .build()?;

    println!("Structured Output Agent created: {}", agent.name());
    println!("This agent will always respond with JSON data.");
    println!("Try asking: 'What is the weather in Tokyo?'");

    // Run with the default launcher
    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}
