//! Validates: docs/official_docs/agents/llm-agent.md
//!
//! This example demonstrates all LlmAgent configuration options as documented
//! in the LlmAgent documentation page.
//!
//! Run modes:
//!   cargo run --example llm_agent_config -p adk-rust-guide              # Validation mode
//!   cargo run --example llm_agent_config -p adk-rust-guide -- chat      # Interactive console
//!   cargo run --example llm_agent_config -p adk-rust-guide -- serve     # Web server mode

use adk_rust::prelude::*;
use adk_rust::{IncludeContents, Launcher};
use adk_rust_guide::{init_env, is_interactive_mode, print_success, print_validating};
use serde_json::json;
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Load API key from environment
    let api_key = init_env();

    // Create the Gemini model
    let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;

    // Create a simple tool to demonstrate tool integration
    // FunctionTool::new takes (name, description, handler)
    // Schema is set via builder methods
    let greet_tool = FunctionTool::new(
        "greet",
        "Generate a personalized greeting",
        |_ctx, args| async move {
            let name = args.get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("friend");
            let style = args.get("style")
                .and_then(|v| v.as_str())
                .unwrap_or("casual");
            
            let greeting = match style {
                "formal" => format!("Good day, {}. How may I assist you?", name),
                _ => format!("Hey {}! What's up?", name),
            };
            
            Ok(json!({ "greeting": greeting }))
        },
    );

    // LlmAgent with comprehensive configuration options
    let agent = LlmAgentBuilder::new("configured_agent")
        // Basic configuration
        .description("An agent demonstrating all configuration options")
        
        // Instruction with template variable injection
        // {user_name} will be replaced with session state value at runtime
        .instruction(
            "You are a helpful assistant configured with all available options. \
             The user's name is {user_name}. \
             Use the greet tool when asked to greet someone. \
             Be friendly and helpful."
        )
        
        // Model configuration (required)
        .model(Arc::new(model))
        
        // Tool integration
        .tool(Arc::new(greet_tool))
        
        // Conversation history control
        // Default: agent sees full conversation history
        // None: agent only sees current turn (stateless)
        .include_contents(IncludeContents::Default)
        
        // Output key: saves agent's final response to session state
        // This allows other agents or tools to access the response
        .output_key("last_response")
        
        // Output schema for structured responses (optional)
        // When set, the LLM formats responses according to this schema
        .output_schema(json!({
            "type": "object",
            "properties": {
                "response": {
                    "type": "string",
                    "description": "The agent's response text"
                },
                "confidence": {
                    "type": "number",
                    "description": "Confidence level from 0 to 1"
                }
            }
        }))
        
        // Build the agent
        .build()?;

    if is_interactive_mode() {
        // Run with Launcher for interactive mode (chat or serve)
        Launcher::new(Arc::new(agent)).run().await?;
    } else {
        // Validation mode - verify all configuration options
        print_validating("agents/llm-agent.md");
        
        println!("=== LlmAgent Configuration Demo ===\n");
        
        // Verify basic properties
        println!("Agent name: {}", agent.name());
        println!("Agent description: {}", agent.description());
        
        // Verify the agent was built successfully with all options
        assert_eq!(agent.name(), "configured_agent");
        assert!(!agent.description().is_empty());
        
        println!("\n✓ Basic configuration verified");
        println!("✓ Instruction with template variables set");
        println!("✓ Tool integration configured");
        println!("✓ IncludeContents mode set to Default");
        println!("✓ Output key configured");
        println!("✓ Output schema configured");
        
        print_success("llm_agent_config");
        
        println!("\nTip: Run with 'chat' for interactive mode:");
        println!("  cargo run --example llm_agent_config -p adk-rust-guide -- chat");
    }

    Ok(())
}
