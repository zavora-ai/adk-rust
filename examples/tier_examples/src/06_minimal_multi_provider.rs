//! # 06 — Minimal Multi-Provider Auto-Detect
//!
//! All three major providers (Gemini, OpenAI, Anthropic) are in the minimal tier.
//! Uses `provider_from_env()` to auto-detect which API key is set.
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
    let model = provider_from_env()?;
    println!("Auto-detected provider from environment.\n");

    let agent = LlmAgentBuilder::new("multi-provider-agent")
        .instruction("You are a helpful assistant. Tell the user which model you are.")
        .model(model)
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
