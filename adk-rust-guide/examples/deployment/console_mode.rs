//! Validates: docs/official_docs/deployment/launcher.md
//!
//! This example demonstrates running an agent in console mode.

use adk_rust::prelude::*;
use adk_rust::Launcher;
use adk_rust_guide::{init_env, is_interactive_mode, print_success, print_validating};
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = init_env();
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?);

    // Create agent
    let agent = LlmAgentBuilder::new("console_agent")
        .description("An agent for console interaction")
        .instruction("You are a helpful assistant. Be concise and friendly.")
        .model(model)
        .build()?;

    // Create launcher for console mode
    let launcher = Launcher::new(Arc::new(agent));

    if is_interactive_mode() {
        // Run in interactive console mode
        launcher.run().await?;
    } else {
        // Validation mode
        print_validating("deployment/launcher.md");
        
        println!("✓ Launcher created successfully");
        println!("✓ Console mode configured");
        println!("\nConsole mode features:");
        println!("  - Interactive REPL loop");
        println!("  - Real-time streaming responses");
        println!("  - Multi-agent transfer support");
        println!("  - Type 'exit' to quit");
        
        print_success("console_mode");
        println!("\nTip: Run with 'chat' for interactive mode:");
        println!("  cargo run --example console_mode -p adk-rust-guide -- chat");
    }

    Ok(())
}
