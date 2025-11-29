//! Validates: docs/official_docs/agents/workflow-agents.md
//!
//! This example demonstrates LoopAgent for iterative refinement.
//! LoopAgent runs sub-agents repeatedly until an exit condition is met
//! (via ExitLoopTool) or max iterations are reached.
//!
//! Run modes:
//!   cargo run --example loop_agent -p adk-rust-guide              # Validation mode
//!   cargo run --example loop_agent -p adk-rust-guide -- chat      # Interactive console
//!   cargo run --example loop_agent -p adk-rust-guide -- serve     # Web server mode

use adk_rust::prelude::*;
use adk_rust::Launcher;
use adk_rust_guide::{init_env, is_interactive_mode, print_success, print_validating};
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Load API key from environment
    let api_key = init_env();

    // Create the Gemini model
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?);

    // Create an agent that iteratively refines content
    // The agent has access to ExitLoopTool to signal when refinement is complete
    let refiner = LlmAgentBuilder::new("refiner")
        .description("Iteratively refines and improves content")
        .instruction(
            "You are a content refiner. Review the content and improve it. \
             \n\nEvaluation criteria: \
             \n1. Clarity - Is the message clear and easy to understand? \
             \n2. Conciseness - Is it free of unnecessary words? \
             \n3. Structure - Is it well-organized? \
             \n4. Grammar - Is it grammatically correct? \
             \n\nIf the content meets ALL criteria, call the exit_loop tool to finish. \
             Otherwise, provide an improved version and explain what you changed."
        )
        .model(model.clone())
        .tool(Arc::new(ExitLoopTool::new()))
        .build()?;

    // Create the loop agent with max iterations for safety
    // The loop will run until:
    // 1. The refiner calls exit_loop (content is good enough), OR
    // 2. Max iterations (5) are reached
    let loop_agent = LoopAgent::new(
        "iterative_refiner",
        vec![Arc::new(refiner)],
    )
    .with_description("Iteratively refines content until quality threshold is met")
    .with_max_iterations(5);

    if is_interactive_mode() {
        // Run with Launcher for interactive mode (chat or serve)
        Launcher::new(Arc::new(loop_agent)).run().await?;
    } else {
        // Validation mode - verify the agent was created correctly
        print_validating("agents/workflow-agents.md");

        // Verify agent properties
        println!("Agent name: {}", loop_agent.name());
        println!("Agent description: {}", loop_agent.description());
        println!("Number of sub-agents: {}", loop_agent.sub_agents().len());

        // Verify the loop agent was built correctly
        assert_eq!(loop_agent.name(), "iterative_refiner");
        assert_eq!(loop_agent.sub_agents().len(), 1);

        // Verify the sub-agent has the ExitLoopTool
        let refiner_agent = &loop_agent.sub_agents()[0];
        assert_eq!(refiner_agent.name(), "refiner");

        print_success("loop_agent");

        println!("\nTip: Run with 'chat' for interactive mode:");
        println!("  cargo run --example loop_agent -p adk-rust-guide -- chat");
        println!("\nTry asking: 'Improve this text: The quick brown fox jumps over the lazy dog it was very fast and the dog was sleeping'");
    }

    Ok(())
}
