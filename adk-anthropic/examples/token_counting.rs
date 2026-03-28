//! Token counting with the Anthropic Messages API.
//!
//! Demonstrates using the `/v1/messages/count_tokens` endpoint to estimate
//! input token counts before sending requests. Useful for:
//! - Proactively managing rate limits and costs
//! - Making smart model routing decisions
//! - Optimizing prompt length
//!
//! Token counting is free but subject to RPM rate limits.
//!
//! Run: `ANTHROPIC_API_KEY=sk-... cargo run -p adk-anthropic --example token_counting`

use adk_anthropic::{
    Anthropic, KnownModel, MessageCountTokensParams, MessageParam, MessageRole, Model,
    ThinkingConfig, ToolUnionParam,
};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();

    let client = Anthropic::new(None)?;

    // ── 1. Basic message ─────────────────────────────────────────
    println!("=== Basic Message ===\n");

    let params = MessageCountTokensParams::new(
        vec![MessageParam::new_with_string("Hello, Claude".to_string(), MessageRole::User)],
        Model::Known(KnownModel::ClaudeOpus46),
    )
    .with_system_string("You are a scientist".to_string());

    let count = client.count_tokens(params).await?;
    println!("Input tokens: {}\n", count.input_tokens);

    // ── 2. Message with tools ────────────────────────────────────
    println!("=== Message with Tools ===\n");

    let tool = ToolUnionParam::new_custom_tool(
        "get_weather".to_string(),
        json!({
            "type": "object",
            "properties": {
                "location": {
                    "type": "string",
                    "description": "The city and state, e.g. San Francisco, CA"
                }
            },
            "required": ["location"]
        }),
    );

    let params = MessageCountTokensParams::new(
        vec![MessageParam::new_with_string(
            "What's the weather like in San Francisco?".to_string(),
            MessageRole::User,
        )],
        Model::Known(KnownModel::ClaudeOpus46),
    )
    .with_tools(vec![tool]);

    let count = client.count_tokens(params).await?;
    println!("Input tokens (with tool): {}\n", count.input_tokens);

    // ── 3. With extended thinking enabled ──────────────────────────
    println!("=== With Extended Thinking ===\n");

    let params = MessageCountTokensParams::new(
        vec![MessageParam::new_with_string(
            "Prove that the square root of 2 is irrational.".to_string(),
            MessageRole::User,
        )],
        Model::Known(KnownModel::ClaudeSonnet46),
    )
    .with_thinking(ThinkingConfig::enabled(16000));

    let count = client.count_tokens(params).await?;
    println!("Input tokens (with thinking enabled): {}\n", count.input_tokens);
    println!("Note: thinking adds a system prompt overhead.\n");

    // ── 4. Compare prompt sizes for model routing ────────────────
    println!("=== Prompt Size Comparison for Model Routing ===\n");

    let short_prompt = "What is 2+2?";
    let long_prompt = "Analyze the following code and suggest improvements for performance, \
        readability, and maintainability. Consider edge cases, error handling, \
        and potential security vulnerabilities. Also suggest appropriate test cases \
        that would cover the main functionality and edge cases. Here is the code: \
        fn process_data(input: &[u8]) -> Result<Vec<String>, Box<dyn std::error::Error>> { \
            let mut results = Vec::new(); \
            for chunk in input.chunks(1024) { \
                let s = std::str::from_utf8(chunk)?; \
                results.push(s.to_uppercase()); \
            } \
            Ok(results) \
        }";

    for (label, prompt) in [("Short", short_prompt), ("Long", long_prompt)] {
        let params = MessageCountTokensParams::new(
            vec![MessageParam::new_with_string(prompt.to_string(), MessageRole::User)],
            Model::Known(KnownModel::ClaudeSonnet46),
        );
        let count = client.count_tokens(params).await?;

        let model_suggestion = if count.input_tokens < 100 {
            "Haiku 4.5 (fast, cheap)"
        } else if count.input_tokens < 1000 {
            "Sonnet 4.6 (balanced)"
        } else {
            "Opus 4.6 (complex reasoning)"
        };

        println!(
            "{label} prompt: {} tokens → suggested model: {model_suggestion}",
            count.input_tokens
        );
    }

    Ok(())
}
