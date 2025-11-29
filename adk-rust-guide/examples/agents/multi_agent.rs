//! Validates: docs/official_docs/agents/multi-agent.md
//!
//! This example demonstrates multi-agent hierarchies with sub-agents.

use adk_rust::prelude::*;
use adk_rust_guide::{init_env, print_success, print_validating};
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    print_validating("agents/multi-agent.md");

    let api_key = init_env();
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?);

    // Create specialist sub-agents
    let math_agent = LlmAgentBuilder::new("math_specialist")
        .description("Handles mathematical calculations")
        .instruction("You are a math specialist. Solve mathematical problems.")
        .model(model.clone())
        .build()?;

    let writing_agent = LlmAgentBuilder::new("writing_specialist")
        .description("Handles writing tasks")
        .instruction("You are a writing specialist. Help with writing tasks.")
        .model(model.clone())
        .build()?;

    // Create coordinator agent with sub-agents
    let coordinator = LlmAgentBuilder::new("coordinator")
        .description("Coordinates between specialist agents")
        .instruction("You coordinate tasks. Delegate to math_specialist or writing_specialist as needed.")
        .model(model.clone())
        .sub_agent(Arc::new(math_agent))
        .sub_agent(Arc::new(writing_agent))
        .build()?;

    println!("Created multi-agent system with coordinator: {}", coordinator.name());

    print_success("multi_agent");
    Ok(())
}
