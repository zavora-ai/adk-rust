//! Structured Output - JSON schema for structured responses

use adk_rust::prelude::*;
use adk_rust::Launcher;
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    // Agent with JSON schema for structured output
    let extractor = LlmAgentBuilder::new("entity_extractor")
        .instruction("Extract entities from the given text. Return JSON with people, locations, and dates.")
        .model(Arc::new(model))
        .output_schema(json!({
            "type": "object",
            "properties": {
                "people": {
                    "type": "array",
                    "items": { "type": "string" }
                },
                "locations": {
                    "type": "array", 
                    "items": { "type": "string" }
                },
                "dates": {
                    "type": "array",
                    "items": { "type": "string" }
                }
            },
            "required": ["people", "locations", "dates"]
        }))
        .build()?;

    println!("📋 Structured Output Demo");
    println!();
    println!("This agent extracts entities and returns structured JSON.");
    println!();
    println!("Try: 'John met Sarah in Paris on December 25th'");
    println!();
    println!("Expected output format:");
    println!("{{");
    println!("  \"people\": [\"John\", \"Sarah\"],");
    println!("  \"locations\": [\"Paris\"],");
    println!("  \"dates\": [\"December 25th\"]");
    println!("}}");
    println!();

    Launcher::new(Arc::new(extractor)).run().await?;
    Ok(())
}
