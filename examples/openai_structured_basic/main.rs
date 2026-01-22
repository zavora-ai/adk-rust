//! OpenAI Structured Output - Basic Example
//!
//! Demonstrates basic structured JSON output with OpenAI.
//!
//! ```bash
//! OPENAI_API_KEY=your-key cargo run --example openai_structured_basic --features openai
//! ```

use adk_agent::LlmAgentBuilder;
use adk_cli::Launcher;
use adk_core::Agent;
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key =
        std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY environment variable required");

    let model_name = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());

    let config = OpenAIConfig::new(&api_key, &model_name);
    let model = OpenAIClient::new(config)?;

    let agent = LlmAgentBuilder::new("data_extractor")
        .description("Extracts person information from text")
        .model(Arc::new(model))
        .instruction("Extract person information from the text provided by the user.")
        .output_schema(json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Full name of the person" },
                "age": { "type": "number", "description": "Age in years" },
                "email": { "type": "string", "description": "Email address if mentioned" }
            },
            "required": ["name", "age"]
        }))
        .build()?;

    println!("OpenAI Structured Output - Basic Example");
    println!("Agent: {}", agent.name());
    println!("Model: {}", model_name);
    println!("\nTry: 'John Doe is 30 years old and can be reached at john@example.com'\n");

    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}
