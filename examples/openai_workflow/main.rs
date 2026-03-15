//! OpenAI Workflow example with ADK.
//!
//! This example demonstrates using OpenAI with sequential agent workflows.
//!
//! Set OPENAI_API_KEY environment variable before running:
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! cargo run --example openai_workflow --features openai
//! ```

use adk_agent::{LlmAgentBuilder, SequentialAgent};
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use anyhow::Result;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");

    // Step 1: Analyze the topic
    let analyzer = LlmAgentBuilder::new("analyzer")
        .description("Analyzes topics")
        .instruction("Analyze the given topic and identify 3-5 key points. Be concise.")
        .model(Arc::new(OpenAIClient::new(OpenAIConfig::new(api_key.clone(), "gpt-5-mini"))?))
        .build()?;

    // Step 2: Expand on the analysis
    let expander = LlmAgentBuilder::new("expander")
        .description("Expands on analysis")
        .instruction("Take the analysis and expand on each key point with 1-2 sentences of detail.")
        .model(Arc::new(OpenAIClient::new(OpenAIConfig::new(api_key.clone(), "gpt-5-mini"))?))
        .build()?;

    // Step 3: Summarize
    let summarizer = LlmAgentBuilder::new("summarizer")
        .description("Summarizes content")
        .instruction("Create a concise 2-3 sentence summary of the expanded analysis.")
        .model(Arc::new(OpenAIClient::new(OpenAIConfig::new(api_key, "gpt-5-mini"))?))
        .build()?;

    let sequential = SequentialAgent::new(
        "openai_workflow",
        vec![Arc::new(analyzer), Arc::new(expander), Arc::new(summarizer)],
    );

    println!("OpenAI Sequential Workflow Agent");
    println!("================================");
    println!("This agent processes input through 3 stages:");
    println!("  1. Analyzer - identifies key points");
    println!("  2. Expander - adds detail to each point");
    println!("  3. Summarizer - creates final summary\n");

    adk_cli::console::run_console(
        Arc::new(sequential),
        "openai_workflow_app".to_string(),
        "user1".to_string(),
    )
    .await?;

    Ok(())
}
