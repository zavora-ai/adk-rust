//! OpenAI Structured Output - Strict Schema Example
//!
//! Demonstrates strict structured JSON output with nested objects.
//! Uses additionalProperties: false at all levels for full OpenAI strict mode.
//!
//! ```bash
//! OPENAI_API_KEY=your-key cargo run --example openai_structured_strict --features openai
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

    let model_name = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-5-mini".to_string());

    let config = OpenAIConfig::new(&api_key, &model_name);
    let model = OpenAIClient::new(config)?;

    // Strict schema with nested objects - additionalProperties: false at every level
    let agent = LlmAgentBuilder::new("idea_extractor")
        .description("Extracts idea specifications from user input")
        .model(Arc::new(model))
        .instruction(
            "Extract the idea specification from the user's description. \
             Determine priority based on urgency keywords (urgent/asap = high, \
             soon/next = medium, eventually/someday = low).",
        )
        .output_schema(json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "Short title for the idea"
                },
                "description": {
                    "type": "string",
                    "description": "Detailed description of the idea"
                },
                "priority": {
                    "type": "string",
                    "enum": ["low", "medium", "high"],
                    "description": "Priority level based on urgency"
                },
                "metadata": {
                    "type": "object",
                    "properties": {
                        "category": {
                            "type": "string",
                            "description": "Category of the idea (feature, bugfix, improvement)"
                        },
                        "estimated_effort": {
                            "type": "string",
                            "enum": ["small", "medium", "large"],
                            "description": "Estimated implementation effort"
                        },
                        "tags": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Relevant tags for the idea"
                        }
                    },
                    "required": ["category", "estimated_effort", "tags"],
                    "additionalProperties": false
                }
            },
            "required": ["title", "description", "priority", "metadata"],
            "additionalProperties": false
        }))
        .build()?;

    println!("OpenAI Structured Output - Strict Schema Example");
    println!("Agent: {}", agent.name());
    println!("Model: {}", model_name);
    println!("\nThis example uses strict schema with nested objects.");
    println!("Try: 'We urgently need a REST API for user management with authentication'\n");

    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}
