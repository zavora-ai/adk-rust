//! mistral.rs In-Situ Quantization (ISQ) example.
//!
//! This example demonstrates how to use ISQ to quantize models on-the-fly
//! at load time, reducing memory usage significantly.
//!
//! # What is ISQ?
//!
//! In-Situ Quantization (ISQ) allows you to load a full-precision model
//! and quantize it during loading. This means you can:
//! - Use any HuggingFace model without pre-quantized versions
//! - Choose your quantization level based on your hardware
//! - Trade off between quality and memory usage
//!
//! # Quantization Levels
//!
//! | Level | Bits | Memory Reduction | Quality |
//! |-------|------|------------------|---------|
//! | Q2K   | 2    | ~88%             | Lower   |
//! | Q3K   | 3    | ~81%             | Low     |
//! | Q4_0  | 4    | ~75%             | Good    |
//! | Q4K   | 4    | ~75%             | Good    |
//! | Q5K   | 5    | ~69%             | Better  |
//! | Q6K   | 6    | ~63%             | High    |
//! | Q8_0  | 8    | ~50%             | Best    |
//!
//! # Running
//!
//! ```bash
//! cargo run --example mistralrs_isq
//! ```

use adk_agent::LlmAgentBuilder;
use adk_mistralrs::{MistralRsConfig, MistralRsModel, ModelSource, QuantizationLevel};
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

    println!("ADK mistral.rs ISQ (In-Situ Quantization) Example");
    println!("==================================================");
    println!();

    // Get model ID and quantization level from environment
    let model_id = std::env::var("MISTRALRS_MODEL")
        .unwrap_or_else(|_| "microsoft/Phi-3.5-mini-instruct".to_string());

    let quant_level = std::env::var("QUANT_LEVEL").unwrap_or_else(|_| "Q4_0".to_string());

    let quantization = match quant_level.to_uppercase().as_str() {
        "Q2K" => QuantizationLevel::Q2K,
        "Q3K" => QuantizationLevel::Q3K,
        "Q4_0" => QuantizationLevel::Q4_0,
        "Q4_1" => QuantizationLevel::Q4_1,
        "Q4K" => QuantizationLevel::Q4K,
        "Q5_0" => QuantizationLevel::Q5_0,
        "Q5_1" => QuantizationLevel::Q5_1,
        "Q5K" => QuantizationLevel::Q5K,
        "Q6K" => QuantizationLevel::Q6K,
        "Q8_0" => QuantizationLevel::Q8_0,
        "Q8_1" => QuantizationLevel::Q8_1,
        _ => {
            println!("Unknown quantization level: {}. Using Q4_0", quant_level);
            QuantizationLevel::Q4_0
        }
    };

    println!("Model: {}", model_id);
    println!("Quantization: {:?}", quantization);
    println!();
    println!("Loading and quantizing model...");
    println!("(This may take longer than normal loading due to quantization)");
    println!();

    // Create model configuration with ISQ
    let config = MistralRsConfig::builder()
        .model_source(ModelSource::huggingface(&model_id))
        .isq(quantization)
        .paged_attention(true) // Enable PagedAttention for additional memory savings
        .temperature(0.7)
        .max_tokens(1024)
        .build();

    // Load and quantize the model
    let start = std::time::Instant::now();
    let model = MistralRsModel::new(config).await?;
    let load_time = start.elapsed();

    println!("Model loaded and quantized in {:.2}s", load_time.as_secs_f64());
    println!();

    // Print memory usage info
    println!("Memory Optimization Tips:");
    println!("-------------------------");
    println!("- Q4_0/Q4K: Good balance of quality and memory (~75% reduction)");
    println!("- Q8_0: Best quality with moderate memory savings (~50% reduction)");
    println!("- Q2K/Q3K: Maximum memory savings, lower quality");
    println!("- PagedAttention: Enabled for efficient long context handling");
    println!();

    // Create an agent with the quantized model
    let agent = LlmAgentBuilder::new("quantized-assistant")
        .description("An assistant running on a quantized model")
        .model(Arc::new(model))
        .instruction(
            "You are a helpful assistant running on a quantized model. \
             Despite the quantization, you should still provide accurate and helpful responses.",
        )
        .build()?;

    println!("Try asking complex questions to test the quantized model's capabilities!");
    println!();

    // Run interactive console
    adk_cli::console::run_console(
        Arc::new(agent),
        "mistralrs_isq".to_string(),
        "user1".to_string(),
    )
    .await?;

    Ok(())
}
