//! Extended thinking with the Anthropic Messages API.
//!
//! Demonstrates:
//! 1. Adaptive thinking (recommended for 4.6 models) with effort via output_config
//! 2. Budget-based thinking (legacy, works on all models)
//!
//! Run: `ANTHROPIC_API_KEY=sk-... cargo run`

use adk_anthropic::{
    Anthropic, EffortLevel, KnownModel, MessageCreateParams, MessageParam, OutputConfig,
    ThinkingConfig,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();

    let client = Anthropic::new(None)?;

    // --- 1. Adaptive thinking (Sonnet 4.6) + effort in output_config ---
    println!("=== Adaptive Thinking + effort: high ===\n");

    let mut params = MessageCreateParams::simple(
        "What is 27 * 453 + 18? Show your reasoning.",
        KnownModel::ClaudeSonnet46,
    )
    .with_thinking(ThinkingConfig::adaptive());
    params.output_config = Some(OutputConfig::with_effort(EffortLevel::High));

    let response = client.send(params).await?;

    for block in &response.content {
        if let Some(thinking) = block.as_thinking() {
            let preview: String = thinking.thinking.chars().take(300).collect();
            println!("[thinking] {preview}...\n");
        }
        if let Some(text) = block.as_text() {
            println!("[answer] {}", text.text);
        }
    }
    println!(
        "\nUsage: {} in / {} out tokens",
        response.usage.input_tokens, response.usage.output_tokens
    );

    // --- 2. Budget-based thinking (legacy, still works on 4.6) ---
    println!("\n=== Budget-Based Thinking (10000 token budget) ===\n");

    // budget_tokens must be < max_tokens
    let params = MessageCreateParams::new(
        16000,
        vec![MessageParam::from(
            "If a train travels 120km in 1.5 hours, what is its average speed?",
        )],
        KnownModel::ClaudeSonnet46.into(),
    )
    .with_thinking(ThinkingConfig::enabled(10000));

    let response = client.send(params).await?;

    for block in &response.content {
        if let Some(thinking) = block.as_thinking() {
            let preview: String = thinking.thinking.chars().take(300).collect();
            println!("[thinking] {preview}...\n");
        }
        if let Some(text) = block.as_text() {
            println!("[answer] {}", text.text);
        }
    }
    println!(
        "\nUsage: {} in / {} out tokens",
        response.usage.input_tokens, response.usage.output_tokens
    );

    Ok(())
}
