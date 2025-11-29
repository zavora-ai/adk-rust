//! Validates: docs/official_docs/agents/workflow-agents.md
//!
//! This example demonstrates SequentialAgent for multi-step pipelines.
//! SequentialAgent executes sub-agents one after another, passing context
//! between them.
//!
//! Run modes:
//!   cargo run --example sequential_agent -p adk-rust-guide              # Validation mode
//!   cargo run --example sequential_agent -p adk-rust-guide -- chat      # Interactive console
//!   cargo run --example sequential_agent -p adk-rust-guide -- serve     # Web server mode

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

    // Step 1: Analyzer - identifies key points
    let analyzer = LlmAgentBuilder::new("analyzer")
        .description("Analyzes topics and identifies key points")
        .instruction("Analyze the given topic and identify 3-5 key points. Be concise.")
        .model(model.clone())
        .build()?;

    // Step 2: Expander - elaborates on the analysis
    let expander = LlmAgentBuilder::new("expander")
        .description("Expands on analysis with details")
        .instruction(
            "Take the analysis from the previous step and expand on each key point \
             with additional details and examples. Keep each point to 2-3 sentences."
        )
        .model(model.clone())
        .build()?;

    // Step 3: Summarizer - creates final summary
    let summarizer = LlmAgentBuilder::new("summarizer")
        .description("Creates concise summaries")
        .instruction(
            "Create a concise summary of the expanded analysis. \
             The summary should be 2-3 paragraphs that capture the main insights."
        )
        .model(model.clone())
        .build()?;

    // Create the sequential pipeline
    // Agents execute in order: analyzer -> expander -> summarizer
    let sequential = SequentialAgent::new(
        "analysis_pipeline",
        vec![Arc::new(analyzer), Arc::new(expander), Arc::new(summarizer)],
    ).with_description("A three-step analysis pipeline: analyze, expand, summarize");

    if is_interactive_mode() {
        // Run with Launcher for interactive mode (chat or serve)
        Launcher::new(Arc::new(sequential)).run().await?;
    } else {
        // Validation mode - verify the agent was created correctly
        print_validating("agents/workflow-agents.md");

        // Verify agent properties
        println!("Agent name: {}", sequential.name());
        println!("Agent description: {}", sequential.description());
        println!("Number of sub-agents: {}", sequential.sub_agents().len());

        // Verify the sequential agent was built correctly
        assert_eq!(sequential.name(), "analysis_pipeline");
        assert_eq!(sequential.sub_agents().len(), 3);

        // Verify sub-agent names
        let sub_agent_names: Vec<&str> = sequential.sub_agents()
            .iter()
            .map(|a| a.name())
            .collect();
        assert_eq!(sub_agent_names, vec!["analyzer", "expander", "summarizer"]);

        print_success("sequential_agent");

        println!("\nTip: Run with 'chat' for interactive mode:");
        println!("  cargo run --example sequential_agent -p adk-rust-guide -- chat");
        println!("\nTry asking: 'Explain the benefits of renewable energy'");
    }

    Ok(())
}
