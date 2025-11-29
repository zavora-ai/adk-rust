//! Validates: docs/official_docs/callbacks/callbacks.md
//!
//! This example demonstrates the after_agent callback for response modification.
//! The after_agent callback executes after the agent completes and can:
//! - Log agent completions
//! - Modify or filter responses
//! - Perform cleanup operations

use adk_rust::prelude::*;
use adk_rust::Launcher;
use adk_rust_guide::{init_env, is_interactive_mode, print_success, print_validating};
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = init_env();
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?);

    // Create agent with after_agent callback
    let agent = LlmAgentBuilder::new("callback_demo")
        .description("Agent demonstrating after_agent callback")
        .instruction("You are a helpful assistant.")
        .model(model)
        // Add after_agent callback for logging and response modification
        .after_callback(Box::new(|ctx| {
            Box::pin(async move {
                println!("[AFTER_AGENT] Agent '{}' completed", ctx.agent_name());
                println!("[AFTER_AGENT] Session: {}", ctx.session_id());
                
                // Return None to keep the original response
                // Return Some(Content) to replace the response
                Ok(None)
            })
        }))
        .build()?;

    if is_interactive_mode() {
        // Run in interactive mode
        Launcher::new(Arc::new(agent)).run().await?;
    } else {
        print_validating("callbacks/callbacks.md");
        
        // Validation: verify agent was created with callback
        println!("Created agent with after_agent callback: {}", agent.name());
        println!("Agent description: {}", agent.description());
        
        print_success("after_agent");
        println!("\nTip: Run with 'chat' for interactive mode:");
        println!("  cargo run --example after_agent -p adk-rust-guide -- chat");
    }

    Ok(())
}
