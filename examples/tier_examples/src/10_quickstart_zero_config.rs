//! # 10 — Quickstart: Zero-Config Alternative
//!
//! Verbatim from the quickstart.md "Zero-Config Alternative" section.
//! Uses `adk::run()` for the simplest possible agent invocation.
//!
//! ```toml
//! [dependencies]
//! adk-rust = "0.8.0"
//! ```

use adk_rust::run;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    // Detects ANTHROPIC_API_KEY, OPENAI_API_KEY, or GOOGLE_API_KEY automatically
    let response = run("You are a helpful assistant.", "Explain Rust in one sentence.").await?;
    println!("{response}");
    Ok(())
}
