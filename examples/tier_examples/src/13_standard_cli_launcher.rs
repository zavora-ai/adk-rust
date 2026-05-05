//! # 13 — Standard: Full CLI Launcher
//!
//! The `standard` tier includes the full CLI launcher with rustyline history,
//! thinking block rendering, and `--serve` mode for REST API.
//!
//! Run with: `cargo run --bin 13-standard-cli-launcher`
//! Or serve: `cargo run --bin 13-standard-cli-launcher -- serve`
//!
//! ```toml
//! [dependencies]
//! adk-rust = { version = "0.8.0", features = ["standard"] }
//! ```

use adk_rust::Launcher;
use adk_rust::prelude::*;

#[tokio::main]
async fn main() -> AnyhowResult<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    let agent = LlmAgentBuilder::new("assistant")
        .description("A helpful AI assistant with full CLI features")
        .instruction("You are a helpful assistant. Be concise and accurate.")
        .model(Arc::new(model))
        .build()?;

    // In standard tier, Launcher is adk_cli::Launcher with:
    // - rustyline readline with history
    // - `--serve` mode for REST API
    // - thinking block rendering
    // - clap CLI parsing
    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
