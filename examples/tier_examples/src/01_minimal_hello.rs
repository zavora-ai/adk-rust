//! # 01 — Minimal Hello World (`adk::run()` one-liner)
//!
//! Verbatim from the README "Fastest Start" section.
//! Uses default features (minimal tier) — no explicit features needed.
//!
//! ```toml
//! [dependencies]
//! adk-rust = "0.8.2"
//! ```

use adk_rust::run;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    // Minimal defaults to Gemini. Set GOOGLE_API_KEY.
    let response = run("You are a helpful assistant.", "What is 2 + 2?").await?;
    if !response.contains('4') {
        anyhow::bail!("minimal hello response did not pass validation");
    }
    println!("minimal hello completed");
    Ok(())
}
