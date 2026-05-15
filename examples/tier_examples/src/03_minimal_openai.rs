//! # 03 — Opt-in OpenAI Example
//!
//! OpenAI is intentionally outside the default minimal tier. Enable it only
//! when the agent needs OpenAI.
//!
//! ```toml
//! [dependencies]
//! adk-rust = { version = "0.8.2", features = ["openai"] }
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
