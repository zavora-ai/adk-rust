//! Validates: docs/official_docs/quickstart.md
//!
//! This example demonstrates the quickstart guide for ADK-Rust.
//! It creates a simple agent matching the documentation example.
//!
//! Run modes:
//!   cargo run --example quickstart -p adk-rust-guide              # Validation mode (default)
//!   cargo run --example quickstart -p adk-rust-guide -- chat      # Interactive console
//!   cargo run --example quickstart -p adk-rust-guide -- serve     # Web server mode

use adk_rust::prelude::*;
use adk_rust::Launcher;
use adk_rust_guide::{init_env, is_interactive_mode, print_success, print_validating};
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Load API key from environment (matches Step 3 in docs)
    let api_key = init_env();

    // Create the Gemini model (matches Step 4 in docs)
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    // Build the agent (matches Step 4 in docs)
    let agent = LlmAgentBuilder::new("my_assistant")
        .description("A helpful AI assistant")
        .instruction("You are a friendly and helpful assistant. Answer questions clearly and concisely.")
        .model(Arc::new(model))
        .build()?;

    if is_interactive_mode() {
        // Run with Launcher for interactive mode (chat or serve)
        Launcher::new(Arc::new(agent)).run().await?;
    } else {
        // Validation mode - verify the agent was created correctly
        print_validating("quickstart.md");
        println!("Agent '{}' created successfully", agent.name());
        println!("Description: {}", agent.description());
        print_success("quickstart");

        println!("\nTip: Run with 'chat' for interactive mode:");
        println!("  cargo run --example quickstart -p adk-rust-guide -- chat");
    }

    Ok(())
}
