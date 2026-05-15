//! # 02 — Minimal Launcher (Interactive REPL)
//!
//! Verbatim from the README "Basic Example (Gemini)" section.
//! Uses the lightweight Launcher from adk-runner — no server, no rustyline.
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
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    let agent = LlmAgentBuilder::new("assistant")
        .description("Helpful AI assistant")
        .instruction("You are a helpful assistant. Be concise and accurate.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
