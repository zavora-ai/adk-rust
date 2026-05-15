//! # 08 — Quickstart: Scaffolded Project
//!
//! Verbatim from the quickstart.md "Understanding the Generated Code" section.
//! This is exactly what `cargo adk new my_agent` generates.
//!
//! ```toml
//! [dependencies]
//! adk-rust = "0.8.2"
//! ```

use adk_rust::Launcher;
use adk_rust::prelude::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;

    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    let agent = LlmAgentBuilder::new("my_agent")
        .description("A helpful AI assistant")
        .instruction("You are a friendly assistant. Be concise and helpful.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
