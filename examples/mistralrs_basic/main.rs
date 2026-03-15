//! Basic mistral.rs text generation example.
//!
//! This example demonstrates how to use adk-mistralrs for local LLM inference
//! without external dependencies like Ollama.
//!
//! # Prerequisites
//!
//! Add adk-mistralrs to your Cargo.toml via git dependency:
//! ```toml
//! adk-mistralrs = { git = "https://github.com/zavora-ai/adk-rust" }
//! # With Metal (macOS): features = ["metal"]
//! # With CUDA: features = ["cuda"]
//! ```
//!
//! # Running
//!
//! ```bash
//! cargo run --example mistralrs_basic
//! ```
//!
//! # Environment Variables
//!
//! - `MISTRALRS_MODEL`: HuggingFace model ID (default: "microsoft/Phi-3.5-mini-instruct")

use adk_agent::LlmAgentBuilder;
use adk_mistralrs::{MistralRsConfig, MistralRsModel, ModelSource};
use std::sync::Arc;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    println!("ADK mistral.rs Basic Example");
    println!("============================");
    println!();

    // Get model ID from environment or use default
    let model_id = std::env::var("MISTRALRS_MODEL")
        .unwrap_or_else(|_| "microsoft/Phi-3.5-mini-instruct".to_string());

    println!("Loading model: {}", model_id);
    println!("This may take a few minutes on first run (downloading model)...");
    println!();

    // Create model configuration
    let config = MistralRsConfig::builder()
        .model_source(ModelSource::huggingface(&model_id))
        .temperature(0.7)
        .max_tokens(1024)
        .build();

    // Load the model
    let model = MistralRsModel::new(config).await?;

    println!("Model loaded successfully!");
    println!();

    // Create an agent with the model
    let agent = LlmAgentBuilder::new("local-assistant")
        .description("A helpful assistant running locally via mistral.rs")
        .model(Arc::new(model))
        .instruction(
            "You are a helpful, friendly assistant. Be concise and accurate in your responses.",
        )
        .build()?;

    // Run interactive console
    adk_cli::console::run_console(
        Arc::new(agent),
        "mistralrs_basic".to_string(),
        "user1".to_string(),
    )
    .await?;

    Ok(())
}
