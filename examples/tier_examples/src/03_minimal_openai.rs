//! # 03 — Minimal OpenAI Example
//!
//! Verbatim from the README "OpenAI Example" section.
//! OpenAI is included in the minimal tier — no extra features needed.
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
    let api_key = std::env::var("OPENAI_API_KEY")?;
    let model = OpenAIClient::new(OpenAIConfig::new(api_key, "gpt-5-mini"))?;

    let agent = LlmAgentBuilder::new("assistant")
        .instruction("You are a helpful assistant.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
