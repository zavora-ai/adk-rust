//! Basic non-streaming chat with the Anthropic Messages API.
//!
//! Demonstrates: client creation, simple message, response parsing.
//!
//! Run: `ANTHROPIC_API_KEY=sk-... cargo run`

use adk_anthropic::{Anthropic, KnownModel, MessageCreateParams};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();

    let client = Anthropic::new(None)?;

    let params = MessageCreateParams::simple(
        "Explain the Rust ownership model in three sentences.",
        KnownModel::ClaudeSonnet46,
    );

    let response = client.send(params).await?;

    println!("Model: {}", response.model);
    println!("Stop reason: {:?}", response.stop_reason);
    println!(
        "Usage: {} in / {} out tokens",
        response.usage.input_tokens, response.usage.output_tokens
    );
    println!();

    for block in &response.content {
        if let Some(text) = block.as_text() {
            println!("{}", text.text);
        }
    }

    Ok(())
}
