//! OpenAI Parallel example with ADK.
//!
//! This example demonstrates running multiple OpenAI agents in parallel.
//!
//! Set OPENAI_API_KEY environment variable before running:
//! ```bash
//! export OPENAI_API_KEY=sk-...
//! cargo run --example openai_parallel --features openai
//! ```

use adk_agent::{LlmAgentBuilder, ParallelAgent};
use adk_model::openai::{OpenAIClient, OpenAIConfig};
use anyhow::Result;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set");

    // Agent 1: Technical perspective
    let technical = LlmAgentBuilder::new("technical_analyst")
        .description("Provides technical analysis")
        .instruction("Analyze the topic from a technical perspective. Be concise (2-3 sentences).")
        .model(Arc::new(OpenAIClient::new(OpenAIConfig::new(api_key.clone(), "gpt-5-mini"))?))
        .build()?;

    // Agent 2: Business perspective
    let business = LlmAgentBuilder::new("business_analyst")
        .description("Provides business analysis")
        .instruction("Analyze the topic from a business perspective. Be concise (2-3 sentences).")
        .model(Arc::new(OpenAIClient::new(OpenAIConfig::new(api_key.clone(), "gpt-5-mini"))?))
        .build()?;

    // Agent 3: User perspective
    let user = LlmAgentBuilder::new("user_analyst")
        .description("Provides user analysis")
        .instruction(
            "Analyze the topic from a user experience perspective. Be concise (2-3 sentences).",
        )
        .model(Arc::new(OpenAIClient::new(OpenAIConfig::new(api_key, "gpt-5-mini"))?))
        .build()?;

    let parallel = ParallelAgent::new(
        "openai_parallel_workflow",
        vec![Arc::new(technical), Arc::new(business), Arc::new(user)],
    );

    println!("OpenAI Parallel Agent");
    println!("=====================");
    println!("This agent runs 3 analysts in parallel:");
    println!("  1. Technical analyst");
    println!("  2. Business analyst");
    println!("  3. User experience analyst\n");

    adk_cli::console::run_console(
        Arc::new(parallel),
        "openai_parallel_app".to_string(),
        "user1".to_string(),
    )
    .await?;

    Ok(())
}
