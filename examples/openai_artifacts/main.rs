//! OpenAI Load Artifacts Example
//!
//! This example demonstrates the LoadArtifactsTool with OpenAI.
//!
//! Set OPENAI_API_KEY environment variable before running:
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! cargo run --example openai_artifacts --features openai
//! ```

use adk_agent::LlmAgentBuilder;
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use adk_tool::LoadArtifactsTool;
use anyhow::Result;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");

    let model = OpenAIClient::new(OpenAIConfig::new(api_key, "gpt-5-mini"))?;

    let _agent = LlmAgentBuilder::new("artifact_agent")
        .description("Agent that can load and analyze artifacts")
        .instruction(
            "You have access to a load_artifacts tool that can load artifacts by name. \
             Use it when asked to load or access artifacts.",
        )
        .model(Arc::new(model))
        .tool(Arc::new(LoadArtifactsTool::new()))
        .build()?;

    println!("OpenAI LoadArtifactsTool Example");
    println!("=================================");
    println!();
    println!("This example demonstrates the LoadArtifactsTool with OpenAI.");
    println!("The tool allows agents to load artifacts from the artifact service.");
    println!();
    println!("To use this in a real scenario:");
    println!("1. Set up an ArtifactService (InMemory or Database)");
    println!("2. Pre-populate it with artifacts");
    println!("3. Add LoadArtifactsTool to your agent");
    println!("4. The agent can then load artifacts by name");
    println!();
    println!("Agent created successfully with LoadArtifactsTool!");

    Ok(())
}
