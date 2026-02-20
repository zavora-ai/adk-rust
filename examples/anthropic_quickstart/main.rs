//! Anthropic Quickstart
//!
//! Minimal example to get started with Claude in ADK. Creates an agent with
//! a system instruction and drops you into an interactive console.
//!
//! ```bash
//! export ANTHROPIC_API_KEY=sk-ant-...
//! cargo run --example anthropic_quickstart --features anthropic
//! ```

use adk_agent::LlmAgentBuilder;
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use anyhow::Result;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let api_key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY must be set");

    let model = AnthropicClient::new(AnthropicConfig::new(api_key, "claude-sonnet-4-20250514"))?;

    let agent = LlmAgentBuilder::new("claude_agent")
        .description("A helpful assistant powered by Claude")
        .instruction("You are a helpful, concise assistant. Answer questions clearly.")
        .model(Arc::new(model))
        .build()?;

    adk_cli::console::run_console(
        Arc::new(agent),
        "anthropic_quickstart".to_string(),
        "user1".to_string(),
    )
    .await?;

    Ok(())
}
