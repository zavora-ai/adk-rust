//! DeepSeek Sequential Multi-Agent Workflow
//!
//! This example demonstrates a sequential pipeline of DeepSeek agents,
//! where each agent's output feeds into the next.
//!
//! Pipeline: Researcher -> Analyst -> Writer
//!
//! Set DEEPSEEK_API_KEY environment variable before running:
//! ```bash
//! cargo run --example deepseek_sequential --features deepseek
//! ```

use adk_agent::{LlmAgentBuilder, SequentialAgent};
use adk_model::deepseek::{DeepSeekClient, DeepSeekConfig};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Load .env file if present
    dotenvy::dotenv().ok();

    let api_key = std::env::var("DEEPSEEK_API_KEY").expect("DEEPSEEK_API_KEY must be set");

    // Create a shared model instance for all agents
    let model = Arc::new(DeepSeekClient::new(DeepSeekConfig::chat(api_key))?);

    // Step 1: Researcher - Gathers facts and information
    let researcher = LlmAgentBuilder::new("researcher")
        .description("Research specialist that gathers facts")
        .instruction(
            "You are a research specialist. When given a topic, gather and list \
             5-7 key facts, statistics, or important points about it. \
             Format as a numbered list. Be concise but informative.",
        )
        .model(model.clone())
        .build()?;

    // Step 2: Analyst - Analyzes the research
    let analyst = LlmAgentBuilder::new("analyst")
        .description("Analyst that identifies insights and patterns")
        .instruction(
            "You are an analyst. Take the research provided and:\n\
             1. Identify 2-3 key insights or patterns\n\
             2. Note any surprising or notable findings\n\
             3. Suggest implications or conclusions\n\
             Be analytical and thoughtful.",
        )
        .model(model.clone())
        .build()?;

    // Step 3: Writer - Creates final content
    let writer = LlmAgentBuilder::new("writer")
        .description("Writer that creates engaging content")
        .instruction(
            "You are a skilled writer. Take the research and analysis provided \
             and create a well-structured, engaging summary paragraph (150-200 words). \
             Include the key insights and make it accessible to a general audience.",
        )
        .model(model)
        .build()?;

    // Create sequential pipeline
    let sequential = SequentialAgent::new(
        "research_pipeline",
        vec![Arc::new(researcher), Arc::new(analyst), Arc::new(writer)],
    );

    println!("=== DeepSeek Sequential Multi-Agent Demo ===\n");
    println!("Pipeline: Researcher -> Analyst -> Writer\n");
    println!("Each agent processes the output of the previous one.\n");
    println!("Try a topic like: 'The impact of AI on healthcare'\n");

    // Run interactive console
    adk_cli::console::run_console(
        Arc::new(sequential),
        "deepseek_sequential".to_string(),
        "user_1".to_string(),
    )
    .await?;

    Ok(())
}
