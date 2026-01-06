//! Multi-model example - serve multiple models
//!
//! Run: cargo run --bin multimodel

use adk_mistralrs::{MistralRsConfig, MistralRsMultiModel, ModelSource};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let multi = MistralRsMultiModel::new();

    // Add first model
    println!("Loading phi model...");
    let phi_config = MistralRsConfig::builder()
        .model_source(ModelSource::huggingface("microsoft/Phi-3.5-mini-instruct"))
        .build();
    multi.add_model("phi", phi_config).await?;

    // Add second model
    println!("Loading gemma model...");
    let gemma_config = MistralRsConfig::builder()
        .model_source(ModelSource::huggingface("google/gemma-2-2b-it"))
        .build();
    multi.add_model("gemma", gemma_config).await?;

    // Set default
    multi.set_default("phi").await?;

    println!("Available models: {:?}", multi.model_names().await);
    println!("Default model: {:?}", multi.default_model().await);

    // Use as Llm trait (routes to default)
    println!("\nMulti-model ready. Use generate_with_model() to route requests.");

    Ok(())
}
