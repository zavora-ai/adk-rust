//! Quantized model example - reduced memory usage with ISQ
//!
//! Run: cargo run --bin quantized

use adk_agent::LlmAgentBuilder;
use adk_mistralrs::{Llm, MistralRsConfig, MistralRsModel, ModelSource, QuantizationLevel};
use adk_rust::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load model with 4-bit quantization for reduced memory
    let config = MistralRsConfig::builder()
        .model_source(ModelSource::huggingface("microsoft/Phi-3.5-mini-instruct"))
        .isq(QuantizationLevel::Q4_0) // 4-bit quantization
        .paged_attention(true) // Memory-efficient attention
        .build();

    println!("Loading quantized model...");
    let model = MistralRsModel::new(config).await?;
    println!("Model loaded: {}", model.name());

    let agent = LlmAgentBuilder::new("quantized_assistant")
        .instruction("You are a helpful assistant. Be concise.")
        .model(Arc::new(model))
        .build()?;

    // Run interactive chat
    Launcher::new(Arc::new(agent)).run().await?;

    Ok(())
}
