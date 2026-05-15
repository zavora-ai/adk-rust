//! # 13 — Standard: Lightweight Launcher
//!
//! The `standard` tier keeps the lightweight launcher unless a `cli-*`
//! provider feature is enabled.
//!
//! Run with: `cargo run --bin 13-standard-cli-launcher`
//!
//! ```toml
//! [dependencies]
//! adk-rust = { version = "0.8.2", features = ["standard"] }
//! ```

use adk_rust::Launcher;
use adk_rust::prelude::*;

#[tokio::main]
async fn main() -> AnyhowResult<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-2.5-flash")?;

    let agent = LlmAgentBuilder::new("assistant")
        .description("A helpful AI assistant with the lightweight launcher")
        .instruction("You are a helpful assistant. Be concise and accurate.")
        .model(Arc::new(model))
        .build()?;

    // Full CLI features are available through opt-in `cli-*` features.
    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
