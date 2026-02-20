//! Anthropic Basic Example
//!
//! This example demonstrates basic usage of Anthropic's Claude models with ADK.
//!
//! Set ANTHROPIC_API_KEY environment variable before running:
//! ```bash
//! export ANTHROPIC_API_KEY=sk-ant-...
//! cargo run --example anthropic_basic --features anthropic
//! ```

use adk_agent::LlmAgentBuilder;
use adk_cli::Launcher;
use adk_core::Agent;
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY must be set");

    // Create the Anthropic client with Claude Sonnet
    let model = AnthropicClient::new(AnthropicConfig::new(api_key, "claude-sonnet-4-5-20250929"))?;

    // Create an agent using the Claude model
    let agent = LlmAgentBuilder::new("claude_assistant")
        .description("A helpful AI assistant powered by Claude")
        .model(Arc::new(model))
        .instruction(
            "You are Claude, a helpful AI assistant created by Anthropic. \
             Be helpful, harmless, and honest. Provide clear, accurate responses.",
        )
        .build()?;

    println!("Anthropic Claude Agent created: {}", agent.name());
    println!("Model: claude-sonnet-4-5-20250929");
    println!("\nTry asking Claude a question!\n");

    // Run with the default launcher
    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}
