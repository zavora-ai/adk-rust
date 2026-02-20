//! Anthropic Model Discovery & Token Counting Example
//!
//! Demonstrates two Anthropic-specific APIs:
//! - Model discovery: list available Claude models and get model details
//! - Token counting: count input tokens for a request without generating a response
//!
//! ```bash
//! export ANTHROPIC_API_KEY=sk-ant-...
//! cargo run --example anthropic_models --features anthropic
//! ```

use adk_core::{Content, LlmRequest, Part};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use std::collections::HashMap;

fn make_request(contents: Vec<Content>) -> LlmRequest {
    LlmRequest { model: String::new(), contents, config: None, tools: HashMap::new() }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY must be set");

    let client = AnthropicClient::new(AnthropicConfig::new(api_key, "claude-sonnet-4-20250514"))?;

    // --- Model Discovery ---
    println!("=== Available Claude Models ===\n");

    let models = client.list_models().await?;
    for model in &models {
        println!("  {} — {} (created: {})", model.id, model.display_name, model.created_at);
    }
    println!("\nTotal: {} models\n", models.len());

    // Get details for a specific model
    println!("=== Model Details ===\n");
    match client.get_model("claude-sonnet-4-20250514").await {
        Ok(info) => {
            println!("  ID:           {}", info.id);
            println!("  Display Name: {}", info.display_name);
            println!("  Created:      {}", info.created_at);
        }
        Err(e) => println!("  Could not fetch model details: {e}"),
    }

    // --- Token Counting ---
    println!("\n=== Token Counting ===\n");

    let short_request = make_request(vec![Content {
        role: "user".to_string(),
        parts: vec![Part::Text { text: "Hello, Claude!".to_string() }],
    }]);

    let count = client.count_tokens(&short_request).await?;
    println!("  Short message tokens: {}", count.input_tokens);

    let long_request = make_request(vec![
        Content {
            role: "system".to_string(),
            parts: vec![Part::Text {
                text: "You are a helpful coding assistant specializing in Rust. \
                       Always provide idiomatic, well-documented code examples."
                    .to_string(),
            }],
        },
        Content {
            role: "user".to_string(),
            parts: vec![Part::Text {
                text: "Explain the difference between Box, Rc, and Arc in Rust. \
                       When should I use each one? Provide code examples for each."
                    .to_string(),
            }],
        },
    ]);

    let count = client.count_tokens(&long_request).await?;
    println!("  Longer message tokens: {}", count.input_tokens);

    // --- Rate Limit Info ---
    println!("\n=== Rate Limit Info (after requests) ===\n");
    let rate_info = client.latest_rate_limit_info().await;
    if let Some(remaining) = rate_info.requests_remaining {
        println!("  Requests remaining: {remaining}");
    }
    if let Some(remaining) = rate_info.tokens_remaining {
        println!("  Tokens remaining:   {remaining}");
    }
    if rate_info.requests_remaining.is_none() && rate_info.tokens_remaining.is_none() {
        println!("  (no rate-limit headers received — normal for successful requests)");
    }

    println!("\nDone!");
    Ok(())
}
