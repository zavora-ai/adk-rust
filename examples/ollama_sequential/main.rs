//! Ollama Sequential Multi-Agent Example
//!
//! Demonstrates a sequential workflow where multiple Ollama agents process
//! content in a pipeline: Analyzer -> Expander -> Summarizer
//!
//! Run: cargo run --example ollama_sequential --features ollama

use adk_agent::{LlmAgentBuilder, SequentialAgent};
use adk_model::ollama::{OllamaConfig, OllamaModel};
use anyhow::Result;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    println!("Ollama Sequential Multi-Agent Example");
    println!("======================================\n");

    let model_name = std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "llama3.2".to_string());
    println!("Using model: {}", model_name);
    println!("Make sure: ollama serve && ollama pull {}\n", model_name);

    // Create three agents with different roles
    let analyzer = LlmAgentBuilder::new("analyzer")
        .description("Analyzes topics")
        .instruction("You are the ANALYZER. Start with '[Analyzer]' then analyze the given topic and identify 3-5 key points. Be concise.")
        .model(Arc::new(OllamaModel::new(OllamaConfig::new(&model_name))?))
        .build()?;

    let expander = LlmAgentBuilder::new("expander")
        .description("Expands on analysis")
        .instruction("You are the EXPANDER. Start with '[Expander]' then take the analysis and expand on each key point with one additional detail.")
        .model(Arc::new(OllamaModel::new(OllamaConfig::new(&model_name))?))
        .build()?;

    let summarizer = LlmAgentBuilder::new("summarizer")
        .description("Summarizes content")
        .instruction("You are the SUMMARIZER. Start with '[Summarizer]' then create a brief 2-3 sentence summary of the expanded analysis.")
        .model(Arc::new(OllamaModel::new(OllamaConfig::new(&model_name))?))
        .build()?;

    // Chain them in sequence: analyzer -> expander -> summarizer
    let sequential = SequentialAgent::new(
        "sequential_workflow",
        vec![Arc::new(analyzer), Arc::new(expander), Arc::new(summarizer)],
    );

    adk_cli::console::run_console(
        Arc::new(sequential),
        "ollama_sequential".to_string(),
        "user1".to_string(),
    )
    .await?;

    Ok(())
}
