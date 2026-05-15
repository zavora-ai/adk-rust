//! # 06 — Minimal Provider Auto-Detect
//!
//! The default minimal tier detects Gemini via `GOOGLE_API_KEY`. Add provider
//! features such as `openai` or `anthropic` to widen auto-detection.
//!
//! ```toml
//! [dependencies]
//! adk-rust = "0.8.2"
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
