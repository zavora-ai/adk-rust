//! OpenAI Token Usage & Caching Metadata Example
//!
//! Demonstrates `UsageMetadata` from OpenAI models including:
//! - `prompt_token_count` / `candidates_token_count` / `total_token_count`
//! - `cache_read_input_token_count` — tokens served from OpenAI's automatic
//!   prompt caching (activates for prompts > 1024 tokens with shared prefix)
//! - `thinking_token_count` — reasoning tokens used by o-series models
//!   (o1, o3-mini, o4-mini) for internal chain-of-thought
//!
//! OpenAI caches prompts automatically — no configuration needed. Repeated
//! requests with the same long prefix get a 50% discount on cached tokens.
//!
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! cargo run --example openai_token_usage --features openai
//! ```

use adk_core::{Content, Llm, LlmRequest, Part, UsageMetadata};
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use futures::StreamExt;
use std::collections::HashMap;

fn print_usage(label: &str, usage: &UsageMetadata) {
    println!("--- {label} ---");
    println!("  prompt tokens:       {}", usage.prompt_token_count);
    println!("  candidate tokens:    {}", usage.candidates_token_count);
    println!("  total tokens:        {}", usage.total_token_count);
    if let Some(cached) = usage.cache_read_input_token_count {
        println!("  cached tokens:       {cached}  ← served from prompt cache (50% cheaper)");
    }
    if let Some(thinking) = usage.thinking_token_count {
        println!("  reasoning tokens:    {thinking}  ← internal chain-of-thought");
    }
    println!();
}

async fn send_and_print(
    model: &OpenAIClient,
    prompt: &str,
    label: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let request = LlmRequest {
        model: String::new(),
        contents: vec![Content::new("user").with_text(prompt)],
        config: None,
        tools: HashMap::new(),
    };

    let mut stream = model.generate_content(request, true).await?;
    let mut text = String::new();
    let mut final_usage = None;

    while let Some(result) = stream.next().await {
        let response = result?;
        if let Some(content) = &response.content {
            for part in &content.parts {
                if let Part::Text { text: t } = part {
                    text.push_str(t);
                }
            }
        }
        if response.turn_complete {
            final_usage = response.usage_metadata;
        }
    }

    let preview = &text[..text.len().min(200)];
    println!("  Response: {preview}...\n");

    if let Some(usage) = &final_usage {
        print_usage(label, usage);
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");

    println!("=== OpenAI Token Usage & Caching Demo ===\n");

    // --- Part 1: Standard model (gpt-4o-mini) with prompt caching ---
    println!(">> Part 1: gpt-4o-mini — automatic prompt caching\n");

    let model = OpenAIClient::new(OpenAIConfig::new(&api_key, "gpt-4o-mini"))?;

    // Build a long shared prefix to trigger caching (>1024 tokens)
    let long_context = "The quick brown fox jumps over the lazy dog. ".repeat(80);
    let prompt =
        format!("Context:\n{long_context}\n\nBased on the context above, what animal jumps?");

    // First request — cache miss
    println!("  Request 1 (cache miss expected):");
    send_and_print(&model, &prompt, "Request 1").await?;

    // Second request with same prefix — cache hit expected
    let prompt2 =
        format!("Context:\n{long_context}\n\nBased on the context above, what animal is lazy?");
    println!("  Request 2 (same prefix — cache hit expected):");
    send_and_print(&model, &prompt2, "Request 2").await?;

    // --- Part 2: Reasoning model (o4-mini) with thinking tokens ---
    println!(">> Part 2: o4-mini — reasoning tokens\n");
    println!("  (o-series models use internal reasoning; thinking_token_count tracks the cost)\n");

    let reasoning_model = OpenAIClient::new(OpenAIConfig::new(&api_key, "o4-mini"))?;

    send_and_print(
        &reasoning_model,
        "A bat and a ball cost $1.10 in total. The bat costs $1.00 more than the ball. \
         How much does the ball cost?",
        "Reasoning request",
    )
    .await?;

    println!("=== Summary ===");
    println!("• OpenAI caches prompts automatically for requests > 1024 tokens");
    println!("• Cached tokens appear in cache_read_input_token_count (50% discount)");
    println!("• o-series models report reasoning tokens in thinking_token_count");
    println!("• Reasoning tokens are billed but not visible in the response text");

    Ok(())
}
