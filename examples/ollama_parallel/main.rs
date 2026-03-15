//! Ollama Parallel Multi-Agent Example
//!
//! Demonstrates parallel execution where multiple Ollama agents analyze
//! the same topic from different perspectives simultaneously.
//!
//! Run: cargo run --example ollama_parallel --features ollama

use adk_agent::{LlmAgentBuilder, ParallelAgent};
use adk_model::ollama::{OllamaConfig, OllamaModel};
use anyhow::Result;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    println!("Ollama Parallel Multi-Agent Example");
    println!("====================================\n");

    let model_name = std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "llama3.2".to_string());
    println!("Using model: {}", model_name);
    println!("Make sure: ollama serve && ollama pull {}\n", model_name);

    // Create three analysts with different perspectives
    let technical = LlmAgentBuilder::new("technical_analyst")
        .description("Technical perspective")
        .instruction("You are the TECHNICAL ANALYST. Start your response with '[Technical Analyst]' then analyze the topic from a technical/engineering perspective in 2-3 sentences.")
        .model(Arc::new(OllamaModel::new(OllamaConfig::new(&model_name))?))
        .build()?;

    let business = LlmAgentBuilder::new("business_analyst")
        .description("Business perspective")
        .instruction("You are the BUSINESS ANALYST. Start your response with '[Business Analyst]' then analyze the topic from a business/market perspective in 2-3 sentences.")
        .model(Arc::new(OllamaModel::new(OllamaConfig::new(&model_name))?))
        .build()?;

    let user_exp = LlmAgentBuilder::new("ux_analyst")
        .description("User experience perspective")
        .instruction("You are the UX ANALYST. Start your response with '[UX Analyst]' then analyze the topic from a user experience perspective in 2-3 sentences.")
        .model(Arc::new(OllamaModel::new(OllamaConfig::new(&model_name))?))
        .build()?;

    // Run all three in parallel
    let parallel = ParallelAgent::new(
        "parallel_analysis",
        vec![Arc::new(technical), Arc::new(business), Arc::new(user_exp)],
    );

    adk_cli::console::run_console(
        Arc::new(parallel),
        "ollama_parallel".to_string(),
        "user1".to_string(),
    )
    .await?;

    Ok(())
}
