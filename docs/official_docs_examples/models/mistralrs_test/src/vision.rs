//! Vision model example - image understanding
//!
//! Run: cargo run --bin vision

use adk_mistralrs::{Llm, MistralRsConfig, MistralRsVisionModel, ModelSource};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load vision model
    let config = MistralRsConfig::builder()
        .model_source(ModelSource::huggingface("microsoft/Phi-3.5-vision-instruct"))
        .build();

    println!("Loading vision model...");
    let model = MistralRsVisionModel::new(config).await?;
    println!("Model loaded: {}", model.name());

    // Example: analyze an image
    // let image = image::open("photo.jpg")?;
    // let response = model.generate_with_image("Describe this image.", vec![image]).await?;

    println!("\nVision model ready. Use generate_with_image() to analyze images.");

    Ok(())
}
