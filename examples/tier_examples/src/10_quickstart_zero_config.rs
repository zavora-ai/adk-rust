//! # 10 — Quickstart: Zero-Config Alternative
//!
//! Verbatim from the quickstart.md "Zero-Config Alternative" section.
//! Uses `adk::run()` for the simplest possible agent invocation.
//!
//! ```toml
//! [dependencies]
//! adk-rust = "0.8.2"
//! ```

use adk_rust::run;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    // Minimal defaults to Gemini. Add provider features for OpenAI/Anthropic auto-detection.
    let response = run("You are a helpful assistant.", "Explain Rust in one sentence.").await?;
    println!("{response}");
    Ok(())
}
