//! Basic mistral.rs example - local LLM inference
//!
//! Run: cargo run --bin basic

use adk_agent::LlmAgentBuilder;
use adk_mistralrs::{Llm, MistralRsConfig, MistralRsModel, ModelSource};
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load model from HuggingFace (downloads on first run)
    let config = MistralRsConfig::builder()
        .model_source(ModelSource::huggingface("microsoft/Phi-3.5-mini-instruct"))
        .build();

    println!("Loading model (this may take a while on first run)...");
    let model = MistralRsModel::new(config).await?;
    println!("Model loaded: {}", model.name());

    // Create agent
    let agent = LlmAgentBuilder::new("local_assistant")
        .description("Local AI assistant powered by mistral.rs")
        .instruction("You are a helpful assistant running locally. Be concise.")
        .model(Arc::new(model))
        .build()?;

    // Run interactive chat
    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}
