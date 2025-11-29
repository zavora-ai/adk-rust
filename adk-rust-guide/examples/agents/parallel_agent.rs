//! Validates: docs/official_docs/agents/workflow-agents.md
//!
//! This example demonstrates ParallelAgent for concurrent execution.
//! ParallelAgent runs all sub-agents simultaneously, collecting results
//! as they complete.
//!
//! Run modes:
//!   cargo run --example parallel_agent -p adk-rust-guide              # Validation mode
//!   cargo run --example parallel_agent -p adk-rust-guide -- chat      # Interactive console
//!   cargo run --example parallel_agent -p adk-rust-guide -- serve     # Web server mode

use adk_rust::prelude::*;
use adk_rust::Launcher;
use adk_rust_guide::{init_env, is_interactive_mode, print_success, print_validating};
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Load API key from environment
    let api_key = init_env();

    // Create the Gemini model (shared across agents)
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?);

    // Create agents for parallel multi-perspective analysis
    // Each agent analyzes the same input from a different perspective

    let technical = LlmAgentBuilder::new("technical_analyst")
        .description("Provides technical analysis")
        .instruction(
            "Analyze the topic from a technical perspective. \
             Focus on implementation details, technologies involved, \
             and technical challenges. Keep your response to 2-3 paragraphs."
        )
        .model(model.clone())
        .build()?;

    let business = LlmAgentBuilder::new("business_analyst")
        .description("Provides business analysis")
        .instruction(
            "Analyze the topic from a business perspective. \
             Focus on market impact, cost-benefit analysis, \
             and business opportunities. Keep your response to 2-3 paragraphs."
        )
        .model(model.clone())
        .build()?;

    let user_exp = LlmAgentBuilder::new("ux_analyst")
        .description("Provides user experience analysis")
        .instruction(
            "Analyze the topic from a user experience perspective. \
             Focus on usability, user needs, and potential pain points. \
             Keep your response to 2-3 paragraphs."
        )
        .model(model.clone())
        .build()?;

    // Create the parallel agent
    // All three analysts run concurrently on the same input
    let parallel = ParallelAgent::new(
        "multi_perspective_analysis",
        vec![Arc::new(technical), Arc::new(business), Arc::new(user_exp)],
    ).with_description("Concurrent analysis from technical, business, and UX perspectives");

    if is_interactive_mode() {
        // Run with Launcher for interactive mode (chat or serve)
        Launcher::new(Arc::new(parallel)).run().await?;
    } else {
        // Validation mode - verify the agent was created correctly
        print_validating("agents/workflow-agents.md");

        // Verify agent properties
        println!("Agent name: {}", parallel.name());
        println!("Agent description: {}", parallel.description());
        println!("Number of sub-agents: {}", parallel.sub_agents().len());

        // Verify the parallel agent was built correctly
        assert_eq!(parallel.name(), "multi_perspective_analysis");
        assert_eq!(parallel.sub_agents().len(), 3);

        // Verify sub-agent names
        let sub_agent_names: Vec<&str> = parallel.sub_agents()
            .iter()
            .map(|a| a.name())
            .collect();
        assert_eq!(sub_agent_names, vec!["technical_analyst", "business_analyst", "ux_analyst"]);

        print_success("parallel_agent");

        println!("\nTip: Run with 'chat' for interactive mode:");
        println!("  cargo run --example parallel_agent -p adk-rust-guide -- chat");
        println!("\nTry asking: 'Analyze the impact of AI assistants in software development'");
    }

    Ok(())
}
