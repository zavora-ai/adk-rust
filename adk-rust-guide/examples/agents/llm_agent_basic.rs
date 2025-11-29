//! Validates: docs/official_docs/agents/llm-agent.md
//!
//! This example demonstrates basic LlmAgent creation as documented
//! in the LlmAgent documentation page.
//!
//! Run modes:
//!   cargo run --example llm_agent_basic -p adk-rust-guide              # Validation mode
//!   cargo run --example llm_agent_basic -p adk-rust-guide -- chat      # Interactive console
//!   cargo run --example llm_agent_basic -p adk-rust-guide -- serve     # Web server mode

use adk_rust::prelude::*;
use adk_rust::Launcher;
use adk_rust_guide::{init_env, is_interactive_mode, print_success, print_validating};
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Load API key from environment
    let api_key = init_env();

    // Create the Gemini model
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    // Basic LlmAgent creation - minimal configuration
    // Only name and model are required
    let agent = LlmAgentBuilder::new("basic_agent")
        .description("A basic agent demonstrating minimal LlmAgent setup")
        .instruction("You are a helpful assistant. Answer questions clearly and concisely.")
        .model(Arc::new(model))
        .build()?;

    if is_interactive_mode() {
        // Run with Launcher for interactive mode (chat or serve)
        Launcher::new(Arc::new(agent)).run().await?;
    } else {
        // Validation mode - verify the agent was created correctly
        print_validating("agents/llm-agent.md");
        
        // Verify agent properties
        println!("Agent name: {}", agent.name());
        println!("Agent description: {}", agent.description());
        
        // Verify the agent was built successfully
        assert_eq!(agent.name(), "basic_agent");
        assert!(!agent.description().is_empty());
        
        print_success("llm_agent_basic");
        
        println!("\nTip: Run with 'chat' for interactive mode:");
        println!("  cargo run --example llm_agent_basic -p adk-rust-guide -- chat");
    }

    Ok(())
}
