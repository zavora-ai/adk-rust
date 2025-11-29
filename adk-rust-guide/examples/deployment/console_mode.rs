//! Validates: docs/official_docs/deployment/launcher.md
//!
//! This example demonstrates running an agent in console mode.

use adk_rust::prelude::*;
use adk_rust::Launcher;
use adk_rust_guide::{init_env, print_success, print_validating};
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    print_validating("deployment/launcher.md");

    let api_key = init_env();
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?);

    // Create agent
    let agent = LlmAgentBuilder::new("console_agent")
        .description("An agent for console interaction")
        .instruction("You are a helpful assistant.")
        .model(model)
        .build()?;

    // Create launcher for console mode
    let launcher = Launcher::new(Arc::new(agent));

    println!("Launcher created for console mode");
    println!("To run interactively: launcher.run().await");

    // Note: We don't actually run the launcher in validation
    // as it would block waiting for user input

    print_success("console_mode");
    Ok(())
}
