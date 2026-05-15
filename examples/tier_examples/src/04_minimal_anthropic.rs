//! # 04 — Opt-in Anthropic Example
//!
//! Anthropic is intentionally outside the default minimal tier. Enable it only
//! when the agent needs Claude.
//!
//! ```toml
//! [dependencies]
//! adk-rust = { version = "0.8.2", features = ["anthropic"] }
//! ```

use adk_rust::Launcher;
use adk_rust::prelude::*;

#[tokio::main]
async fn main() -> AnyhowResult<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("ANTHROPIC_API_KEY")?;
    let model = AnthropicClient::new(AnthropicConfig::new(api_key, "claude-sonnet-4-6"))?;

    let agent = LlmAgentBuilder::new("assistant")
        .instruction("You are a helpful assistant.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
