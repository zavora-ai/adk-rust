//! LoRA adapter example - fine-tuned model support
//!
//! Run: cargo run --bin lora

use adk_agent::LlmAgentBuilder;
use adk_mistralrs::{AdapterConfig, Llm, MistralRsAdapterModel, MistralRsConfig, ModelSource};
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load base model with LoRA adapter
    let config = MistralRsConfig::builder()
        .model_source(ModelSource::huggingface("meta-llama/Llama-3.2-3B-Instruct"))
        .adapter(AdapterConfig::lora("username/my-lora-adapter")) // Replace with real adapter
        .build();

    println!("Loading model with LoRA adapter...");
    let model = MistralRsAdapterModel::new(config).await?;
    println!("Model loaded: {}", model.name());
    println!("Available adapters: {:?}", model.available_adapters());

    let agent = LlmAgentBuilder::new("lora_assistant")
        .instruction("You are a helpful assistant with specialized knowledge.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}
