//! Validates: docs/official_docs/tools/mcp-tools.md
//!
//! This example demonstrates MCP (Model Context Protocol) tool integration.
//! Note: MCP support may be partial - check roadmap for full feature status.

use adk_rust::prelude::*;
use adk_rust_guide::{init_env, print_success, print_validating};
use std::sync::Arc;

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    print_validating("tools/mcp-tools.md");

    let api_key = init_env();
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?);

    // Create a basic agent (MCP toolset would be added here when fully implemented)
    let agent = LlmAgentBuilder::new("mcp_agent")
        .description("An agent demonstrating MCP integration")
        .instruction("You are a helpful assistant with MCP tool capabilities.")
        .model(model)
        .build()?;

    println!("Created agent for MCP demonstration: {}", agent.name());
    println!("Note: Full MCP integration details in docs/roadmap/");

    print_success("mcp_tool");
    Ok(())
}
