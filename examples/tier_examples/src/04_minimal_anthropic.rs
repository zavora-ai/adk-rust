//! # 04 — Minimal Anthropic Example
//!
//! Verbatim from the README "Anthropic Example" section.
//! Anthropic is included in the minimal tier — no extra features needed.
//!
//! ```toml
//! [dependencies]
//! adk-rust = "0.8.0"
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
