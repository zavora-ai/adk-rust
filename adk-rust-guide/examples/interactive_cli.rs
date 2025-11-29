//! Interactive CLI example for ADK-Rust
//!
//! This example demonstrates how to run an agent interactively using the Launcher.
//! Unlike other examples, this one always runs in interactive mode.
//!
//! Run modes:
//!   cargo run --example interactive_cli -p adk-rust-guide              # Interactive console (default)
//!   cargo run --example interactive_cli -p adk-rust-guide -- serve     # Web server mode
//!   cargo run --example interactive_cli -p adk-rust-guide -- serve --port 3000

use adk_rust::prelude::*;
use adk_rust::Launcher;
use adk_rust_guide::init_env;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Load API key from environment
    let api_key = init_env();

    // Create the Gemini model
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    // Build the agent
    let agent = LlmAgentBuilder::new("my_assistant")
        .description("A helpful AI assistant")
        .instruction("You are a friendly and helpful assistant. Answer questions clearly and concisely.")
        .model(Arc::new(model))
        .build()?;

    // Always run interactively - Launcher handles CLI arguments:
    // - No args or 'chat': Interactive console mode
    // - 'serve': Web server mode
    // - 'serve --port 3000': Web server on custom port
    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}
