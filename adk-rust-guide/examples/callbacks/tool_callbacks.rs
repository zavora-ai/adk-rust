//! Validates: docs/official_docs/callbacks/callbacks.md
//!
//! This example demonstrates before_tool and after_tool callbacks.
//! Tool callbacks intercept tool execution:
//! - before_tool: Can validate permissions or skip tool execution
//! - after_tool: Can modify or log tool results

use adk_rust::prelude::*;
use adk_rust::Launcher;
use adk_rust_guide::{init_env, is_interactive_mode, print_success, print_validating};
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let api_key = init_env();
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?);

    // Create agent with a tool and tool callbacks
    let agent = LlmAgentBuilder::new("tool_callback_demo")
        .description("Agent demonstrating tool callbacks")
        .instruction("You are a helpful assistant with search capabilities. Use the search tool when asked to find information.")
        .model(model)
        .tool(Arc::new(GoogleSearchTool::new()))
        // Add before_tool callback - executes before tool runs
        .before_tool_callback(Box::new(|ctx| {
            Box::pin(async move {
                println!("[BEFORE_TOOL] Tool execution starting for agent: {}", ctx.agent_name());
                println!("[BEFORE_TOOL] Session: {}", ctx.session_id());
                
                // Return None to continue with tool execution
                // Return Some(Content) to skip tool and return custom result
                Ok(None)
            })
        }))
        // Add after_tool callback - executes after tool completes
        .after_tool_callback(Box::new(|ctx| {
            Box::pin(async move {
                println!("[AFTER_TOOL] Tool execution completed for agent: {}", ctx.agent_name());
                
                // Return None to keep the original tool result
                // Return Some(Content) to modify the result
                Ok(None)
            })
        }))
        .build()?;

    if is_interactive_mode() {
        // Run in interactive mode
        Launcher::new(Arc::new(agent)).run().await?;
    } else {
        print_validating("callbacks/callbacks.md");
        
        // Validation: verify agent was created with callbacks and tool
        println!("Created agent with tool callbacks: {}", agent.name());
        println!("Agent description: {}", agent.description());
        
        print_success("tool_callbacks");
        println!("\nTip: Run with 'chat' for interactive mode:");
        println!("  cargo run --example tool_callbacks -p adk-rust-guide -- chat");
    }

    Ok(())
}
