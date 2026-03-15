//! mistral.rs LoRA adapter example.
//!
//! This example demonstrates how to use LoRA (Low-Rank Adaptation) adapters
//! with mistral.rs for fine-tuned model inference.
//!
//! # What is LoRA?
//!
//! LoRA is a technique for efficiently fine-tuning large language models by
//! adding small trainable matrices to the model's attention layers. This allows:
//! - Efficient fine-tuning with minimal additional parameters
//! - Multiple adapters for different tasks
//! - Hot-swapping adapters at runtime
//!
//! # Prerequisites
//!
//! You need a LoRA adapter from HuggingFace or a local path. Popular adapters:
//! - Code generation adapters
//! - Domain-specific adapters (medical, legal, etc.)
//! - Style/persona adapters
//!
//! # Running
//!
//! ```bash
//! # With a HuggingFace adapter
//! LORA_ADAPTER=username/my-lora-adapter cargo run --example mistralrs_lora
//!
//! # With multiple adapters
//! LORA_ADAPTERS=adapter1,adapter2,adapter3 cargo run --example mistralrs_lora
//! ```

use adk_agent::LlmAgentBuilder;
use adk_mistralrs::{AdapterConfig, MistralRsAdapterModel, MistralRsConfig, ModelSource};
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

    println!("ADK mistral.rs LoRA Adapter Example");
    println!("====================================");
    println!();

    // Get base model ID
    let base_model =
        std::env::var("BASE_MODEL").unwrap_or_else(|_| "meta-llama/Llama-2-7b-hf".to_string());

    // Get adapter(s) from environment
    let adapter_env = std::env::var("LORA_ADAPTER").ok();
    let adapters_env = std::env::var("LORA_ADAPTERS").ok();

    let adapter_config = if let Some(adapters_str) = adapters_env {
        // Multiple adapters
        let adapters: Vec<String> = adapters_str.split(',').map(|s| s.trim().to_string()).collect();
        println!("Loading multiple LoRA adapters: {:?}", adapters);
        AdapterConfig::lora_multi(adapters)
    } else if let Some(adapter) = adapter_env {
        // Single adapter
        println!("Loading LoRA adapter: {}", adapter);
        AdapterConfig::lora(&adapter)
    } else {
        // Demo mode - show usage
        print_usage();
        return Ok(());
    };

    println!("Base model: {}", base_model);
    println!();
    println!("Loading model with adapter(s)...");
    println!("(This may take several minutes on first run)");
    println!();

    // Create model configuration with LoRA adapter
    let config = MistralRsConfig::builder()
        .model_source(ModelSource::huggingface(&base_model))
        .adapter(adapter_config)
        .temperature(0.7)
        .max_tokens(1024)
        .build();

    // Load the adapter model
    let model = MistralRsAdapterModel::new(config).await?;

    println!("Model loaded successfully!");
    println!();

    // Show available adapters
    let available = model.available_adapters();
    println!("Available adapters: {:?}", available);

    if let Some(active) = model.active_adapter().await {
        println!("Active adapter: {}", active);
    }
    println!();

    // Demonstrate adapter swapping if multiple adapters
    if available.len() > 1 {
        println!("Adapter Hot-Swapping Demo:");
        println!("--------------------------");
        for adapter in &available {
            model.swap_adapter(adapter).await?;
            println!("Switched to adapter: {}", adapter);
        }
        // Switch back to first adapter
        if let Some(first) = available.first() {
            model.swap_adapter(first).await?;
        }
        println!();
    }

    // Create an agent with the adapter model
    let agent = LlmAgentBuilder::new("lora-assistant")
        .description("An assistant using a LoRA fine-tuned model")
        .model(Arc::new(model))
        .instruction(
            "You are a helpful assistant running on a LoRA fine-tuned model. \
             Your responses should reflect the specialized training of the adapter.",
        )
        .build()?;

    // Run interactive console
    adk_cli::console::run_console(
        Arc::new(agent),
        "mistralrs_lora".to_string(),
        "user1".to_string(),
    )
    .await?;

    Ok(())
}

fn print_usage() {
    println!("LoRA Adapter Usage");
    println!("==================");
    println!();
    println!("This example requires a LoRA adapter. Set one of these environment variables:");
    println!();
    println!("Single adapter:");
    println!("  LORA_ADAPTER=username/my-adapter cargo run --example mistralrs_lora");
    println!();
    println!("Multiple adapters (for hot-swapping):");
    println!("  LORA_ADAPTERS=adapter1,adapter2 cargo run --example mistralrs_lora");
    println!();
    println!("Optional: Set base model (default: meta-llama/Llama-2-7b-hf):");
    println!(
        "  BASE_MODEL=mistralai/Mistral-7B-v0.1 LORA_ADAPTER=... cargo run --example mistralrs_lora"
    );
    println!();
    println!("Popular LoRA adapters on HuggingFace:");
    println!("  - Code generation adapters");
    println!("  - Instruction-following adapters");
    println!("  - Domain-specific adapters (medical, legal, etc.)");
    println!();
    println!("Note: The adapter must be compatible with the base model architecture.");
}
