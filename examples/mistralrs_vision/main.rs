//! mistral.rs vision model example.
//!
//! This example demonstrates how to use vision-language models with mistral.rs
//! for image understanding tasks.
//!
//! # Prerequisites
//!
//! Add adk-mistralrs to your Cargo.toml via git dependency:
//! ```toml
//! adk-mistralrs = { git = "https://github.com/zavora-ai/adk-rust", features = ["reqwest"] }
//! ```
//!
//! # Running
//!
//! ```bash
//! cargo run --example mistralrs_vision
//! ```
//!
//! # Environment Variables
//!
//! - `MISTRALRS_VISION_MODEL`: Vision model ID (default: "llava-hf/llava-1.5-7b-hf")
//! - `IMAGE_PATH`: Path to image file to analyze

use adk_mistralrs::{MistralRsConfig, MistralRsVisionModel, ModelArchitecture, ModelSource};
use std::path::PathBuf;
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

    println!("ADK mistral.rs Vision Example");
    println!("==============================");
    println!();

    // Get model ID from environment or use default
    let model_id = std::env::var("MISTRALRS_VISION_MODEL")
        .unwrap_or_else(|_| "llava-hf/llava-1.5-7b-hf".to_string());

    println!("Loading vision model: {}", model_id);
    println!("This may take several minutes on first run (downloading model)...");
    println!();

    // Create vision model configuration
    let config = MistralRsConfig::builder()
        .model_source(ModelSource::huggingface(&model_id))
        .architecture(ModelArchitecture::Vision)
        .temperature(0.7)
        .max_tokens(512)
        .build();

    // Load the vision model
    let model = MistralRsVisionModel::new(config).await?;

    println!("Vision model loaded successfully!");
    println!();

    // Get image path from environment or use sample
    let image_path = std::env::var("IMAGE_PATH").ok().map(PathBuf::from);

    if let Some(path) = image_path {
        if path.exists() {
            println!("Analyzing image: {}", path.display());
            println!();

            // Load the image
            let image = image::open(&path)?;

            // Generate description
            let response = model
                .generate_with_image(
                    "Describe this image in detail. What do you see?",
                    vec![image.clone()],
                )
                .await?;

            println!("Image Analysis:");
            println!("---------------");
            println!("{}", response);
            println!();

            // Ask follow-up questions
            println!("Follow-up Analysis:");
            println!("-------------------");

            let colors = model
                .generate_with_image("What are the main colors in this image?", vec![image.clone()])
                .await?;
            println!("Colors: {}", colors);

            let objects = model
                .generate_with_image(
                    "List the main objects or subjects in this image.",
                    vec![image],
                )
                .await?;
            println!("Objects: {}", objects);
        } else {
            println!("Image file not found: {}", path.display());
            println!();
            print_usage();
        }
    } else {
        print_usage();

        // Demo with a sample prompt (no image)
        println!("Demo: Generating a description of a hypothetical image...");
        println!();

        // Note: Without an actual image, the model will generate based on the prompt alone
        // This is just to demonstrate the API
        println!("To analyze an actual image, set the IMAGE_PATH environment variable.");
    }

    Ok(())
}

fn print_usage() {
    println!("Usage:");
    println!("  IMAGE_PATH=/path/to/image.jpg cargo run --example mistralrs_vision");
    println!();
    println!("Supported image formats: JPEG, PNG, WebP");
    println!();
    println!("Example with sample image:");
    println!("  IMAGE_PATH=adk-mistralrs/models/image.jpg cargo run --example mistralrs_vision");
    println!();
    println!("Supported vision models:");
    println!("  - llava-hf/llava-1.5-7b-hf (default)");
    println!("  - llava-hf/llava-v1.6-mistral-7b-hf");
    println!("  - Qwen/Qwen-VL-Chat");
    println!("  - google/gemma-3-4b-it (with vision)");
    println!();
}
