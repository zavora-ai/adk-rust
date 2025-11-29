//! Validates: docs/official_docs/tools/built-in-tools.md
//!
//! This example demonstrates ExitLoopTool - a built-in tool for loop termination
//! control used with LoopAgent.
//!
//! Run modes:
//!   cargo run --example built_in_exit_loop -p adk-rust-guide              # Validation mode
//!   cargo run --example built_in_exit_loop -p adk-rust-guide -- chat      # Interactive console
//!   cargo run --example built_in_exit_loop -p adk-rust-guide -- serve     # Web server mode

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

    // ExitLoopTool - Loop termination control for LoopAgent
    let exit_tool = ExitLoopTool::new();

    // Build agent with ExitLoopTool for iterative refinement
    let refiner_agent = LlmAgentBuilder::new("content_refiner")
        .description("Iteratively improves content quality")
        .instruction(
            "Review the content and improve it. Check for:\n\
             1. Clarity and readability\n\
             2. Grammar and spelling\n\
             3. Logical flow\n\n\
             If the content meets all quality standards, call the exit_loop tool.\n\
             Otherwise, provide an improved version.",
        )
        .model(Arc::new(model))
        .tool(Arc::new(exit_tool))
        .build()?;

    // Create a LoopAgent using the refiner with ExitLoopTool
    let loop_agent =
        LoopAgent::new("iterative_refiner", vec![Arc::new(refiner_agent)]).with_max_iterations(5);

    if is_interactive_mode() {
        // Run with Launcher for interactive mode (chat or serve)
        Launcher::new(Arc::new(loop_agent)).run().await?;
    } else {
        // Validation mode - verify the tool and agent were created correctly
        print_validating("tools/built-in-tools.md");

        println!("\n=== ExitLoopTool ===");

        // Verify ExitLoopTool properties
        let tool = ExitLoopTool::new();
        assert_eq!(tool.name(), "exit_loop");
        assert!(!tool.description().is_empty());

        println!("Tool name: {}", tool.name());
        println!("Tool description: {}", tool.description());
        println!("LoopAgent name: {}", loop_agent.name());

        // Verify loop agent was built successfully
        assert_eq!(loop_agent.name(), "iterative_refiner");

        print_success("built_in_exit_loop");

        println!("\nTip: Run with 'chat' for interactive mode:");
        println!("  cargo run --example built_in_exit_loop -p adk-rust-guide -- chat");
        println!("\nTry asking: 'Please improve this text: The quick brown fox jumps.'");
    }

    Ok(())
}
