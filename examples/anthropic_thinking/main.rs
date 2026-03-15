//! Anthropic Extended Thinking Example
//!
//! Demonstrates Claude's extended thinking mode, which produces chain-of-thought
//! reasoning before the final response. Thinking content appears wrapped in
//! `<thinking>` tags in the streamed output.
//!
//! ```bash
//! export ANTHROPIC_API_KEY=sk-ant-...
//! cargo run --example anthropic_thinking --features anthropic
//! ```

use adk_core::{Content, Llm, LlmRequest, Part};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use futures::StreamExt;
use std::collections::HashMap;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY must be set");

    // Enable extended thinking with an 8192-token budget.
    // This forces temperature=1.0 as required by the Anthropic API.
    let config = AnthropicConfig::new(api_key, "claude-sonnet-4-20250514")
        .with_max_tokens(16384)
        .with_thinking(8192);

    let client = AnthropicClient::new(config)?;

    println!("=== Anthropic Extended Thinking Example ===");
    println!("Model: claude-sonnet-4 (thinking budget: 8192 tokens)\n");

    let request = LlmRequest {
        model: String::new(),
        contents: vec![Content {
            role: "user".to_string(),
            parts: vec![Part::Text {
                text: "A bat and a ball cost $1.10 in total. The bat costs $1.00 more \
                       than the ball. How much does the ball cost? Think carefully."
                    .to_string(),
            }],
        }],
        config: None,
        tools: HashMap::new(),
    };

    println!("Question: A bat and a ball cost $1.10. The bat costs $1.00 more than the ball.");
    println!("          How much does the ball cost?\n");
    println!("--- Response (with thinking) ---\n");

    let mut stream = client.generate_content(request, true).await?;

    while let Some(result) = stream.next().await {
        match result {
            Ok(response) => {
                if let Some(content) = &response.content {
                    for part in &content.parts {
                        if let Part::Text { text } = part {
                            print!("{text}");
                        }
                    }
                }
                if response.turn_complete {
                    println!("\n\n--- Complete ---");
                    if let Some(usage) = &response.usage_metadata {
                        println!(
                            "Tokens — input: {}, output: {}",
                            usage.prompt_token_count, usage.candidates_token_count,
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!("\nError: {e}");
                break;
            }
        }
    }

    Ok(())
}
