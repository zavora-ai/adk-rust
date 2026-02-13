//! Launcher Basic Example
//!
//! Demonstrates the Launcher for running agents with CLI support.
//!
//! Run in console mode (default):
//!   cd doc-test/deployment/launcher_test
//!   GOOGLE_API_KEY=your_key cargo run --bin basic
//!
//! Run in server mode:
//!   GOOGLE_API_KEY=your_key cargo run --bin basic -- serve --port 8080

use adk_rust::Launcher;
use adk_rust::prelude::*;
use std::sync::Arc;

#[tokio::main]
async fn main() -> adk_core::Result<()> {
    dotenvy::dotenv().ok();

    let api_key =
        std::env::var("GOOGLE_API_KEY").expect("GOOGLE_API_KEY environment variable not set");
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    let agent = LlmAgentBuilder::new("launcher_demo")
        .description("A demo agent for the Launcher")
        .instruction("You are a helpful assistant. Be concise.")
        .model(model)
        .build()?;

    // Launcher handles CLI parsing:
    // - No args or `chat`: Interactive console
    // - `serve [--port PORT]`: HTTP server with web UI
    Launcher::new(Arc::new(agent)).app_name("launcher_demo_app").run().await?;

    Ok(())
}
