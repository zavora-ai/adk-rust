//! # 01 — Minimal Hello World
//!
//! Verbatim from the README "Fastest Start" section.
//! Uses default features (minimal tier) — no explicit features needed.
//!
//! ```toml
//! [dependencies]
//! adk-rust = "0.8.0"
//! ```

use adk_rust::run;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    // Set ANTHROPIC_API_KEY, OPENAI_API_KEY, or GOOGLE_API_KEY
    let response = run("You are a helpful assistant.", "What is 2 + 2?").await?;
    println!("{response}");
    Ok(())
}
