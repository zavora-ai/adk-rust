//! Anthropic Streaming Example
//!
//! Demonstrates direct streaming with the Anthropic client, showing how to
//! consume partial responses in real time. Also showcases prompt caching
//! configuration and system prompt routing.
//!
//! ```bash
//! export ANTHROPIC_API_KEY=sk-ant-...
//! cargo run --example anthropic_streaming --features anthropic
//! ```

use adk_core::{Content, GenerateContentConfig, Llm, LlmRequest, Part};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use futures::StreamExt;
use std::collections::HashMap;

fn make_request(contents: Vec<Content>, config: Option<GenerateContentConfig>) -> LlmRequest {
    LlmRequest { model: String::new(), contents, config, tools: HashMap::new() }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY must be set");

    // Configure with prompt caching enabled
    let config =
        AnthropicConfig::new(api_key, "claude-sonnet-4-20250514").with_prompt_caching(true);

    let client = AnthropicClient::new(config)?;

    // Build a request with an explicit system prompt and user message.
    // The adapter routes system-role content to the top-level `system` parameter.
    let request = make_request(
        vec![
            Content {
                role: "system".to_string(),
                parts: vec![Part::Text {
                    text: "You are a concise technical writer. Respond in short, \
                           well-structured paragraphs. Use bullet points for lists."
                        .to_string(),
                }],
            },
            Content {
                role: "user".to_string(),
                parts: vec![Part::Text {
                    text: "Explain the key differences between TCP and UDP in networking. \
                           Keep it under 200 words."
                        .to_string(),
                }],
            },
        ],
        Some(GenerateContentConfig {
            temperature: Some(0.3),
            max_output_tokens: Some(512),
            ..Default::default()
        }),
    );

    // --- Streaming ---
    println!("=== Anthropic Streaming Example ===");
    println!("Model: claude-sonnet-4-5-20250929 (prompt caching enabled)\n");
    println!("--- Streaming response ---\n");

    let mut stream = client.generate_content(request.clone(), true).await?;

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
                    println!("\n\n--- Stream complete ---");
                    if let Some(usage) = &response.usage_metadata {
                        println!(
                            "Tokens — input: {}, output: {}",
                            usage.prompt_token_count, usage.candidates_token_count,
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!("\nStream error: {e}");
                break;
            }
        }
    }

    // --- Non-streaming for comparison ---
    println!("\n--- Non-streaming response ---\n");

    let mut stream = client.generate_content(request, false).await?;
    if let Some(Ok(response)) = stream.next().await {
        if let Some(content) = &response.content {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    println!("{text}");
                }
            }
        }
        if let Some(usage) = &response.usage_metadata {
            println!(
                "\nTokens — input: {}, output: {}",
                usage.prompt_token_count, usage.candidates_token_count,
            );
        }
    }

    println!("\nDone!");
    Ok(())
}
