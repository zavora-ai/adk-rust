//! Validates: docs/official_docs/callbacks/callbacks.md
//!
//! This example demonstrates the before_agent callback for agent interception.
//! The before_agent callback executes before the agent starts processing and can:
//! - Log agent invocations
//! - Validate input
//! - Return early with a custom response (skip agent execution)

use adk_rust::prelude::*;
use adk_rust::Launcher;
use adk_rust_guide::{init_env, is_interactive_mode, print_success, print_validating};
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = init_env();
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?);

    // Create agent with before_agent callback
    let agent = LlmAgentBuilder::new("callback_demo")
        .description("Agent demonstrating before_agent callback")
        .instruction("You are a helpful assistant.")
        .model(model)
        // Add before_agent callback for logging
        .before_callback(Box::new(|ctx| {
            Box::pin(async move {
                println!("[BEFORE_AGENT] Agent '{}' starting", ctx.agent_name());
                println!("[BEFORE_AGENT] Session: {}", ctx.session_id());
                println!("[BEFORE_AGENT] User: {}", ctx.user_id());
                
                // Return None to continue normal execution
                // Return Some(Content) to skip agent and return early
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
        println!("Created agent with before_agent callback: {}", agent.name());
        println!("Agent description: {}", agent.description());
        
        print_success("before_agent");
        println!("\nTip: Run with 'chat' for interactive mode:");
        println!("  cargo run --example before_agent -p adk-rust-guide -- chat");
    }

    Ok(())
}
