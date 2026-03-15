//! mistral.rs image generation (diffusion) example.
//!
//! This example demonstrates how to use diffusion models with mistral.rs
//! for text-to-image generation using FLUX models.
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
//! # Hardware Requirements
//!
//! FLUX models require significant GPU memory:
//! - FLUX.1-schnell (offloaded): ~12GB VRAM
//! - FLUX.1-schnell (full): ~24GB VRAM
//! - FLUX.1-dev: ~24GB+ VRAM
//!
//! # Running
//!
//! ```bash
//! cargo run --example mistralrs_diffusion
//! ```
//!
//! # Environment Variables
//!
//! - `DIFFUSION_MODEL`: HuggingFace model ID (default: "black-forest-labs/FLUX.1-schnell")
//! - `OUTPUT_DIR`: Directory to save images (default: "./output")
//! - `IMAGE_WIDTH`: Image width in pixels (default: 1024)
//! - `IMAGE_HEIGHT`: Image height in pixels (default: 1024)

use adk_mistralrs::{
    DiffusionConfig, DiffusionModelType, DiffusionParams, MistralRsDiffusionModel, ModelSource,
};
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

    println!("ADK mistral.rs Diffusion (Image Generation) Example");
    println!("====================================================");
    println!();

    // Get model ID from environment or use default
    let model_id = std::env::var("DIFFUSION_MODEL")
        .unwrap_or_else(|_| "black-forest-labs/FLUX.1-schnell".to_string());

    // Get output directory
    let output_dir = std::env::var("OUTPUT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./output"));

    // Get image dimensions
    let width: u32 = std::env::var("IMAGE_WIDTH").ok().and_then(|s| s.parse().ok()).unwrap_or(1024);
    let height: u32 =
        std::env::var("IMAGE_HEIGHT").ok().and_then(|s| s.parse().ok()).unwrap_or(1024);

    // Create output directory if it doesn't exist
    std::fs::create_dir_all(&output_dir)?;

    println!("Model: {}", model_id);
    println!("Output directory: {}", output_dir.display());
    println!("Image size: {}x{}", width, height);
    println!();
    println!("Hardware Requirements:");
    println!("  - FLUX.1-schnell (offloaded): ~12GB VRAM");
    println!("  - FLUX.1-schnell (full): ~24GB VRAM");
    println!();
    println!("Loading diffusion model...");
    println!("This may take several minutes on first run (downloading model)...");
    println!();

    // Create diffusion model configuration
    // Use FluxOffloaded for reduced memory usage
    let config = DiffusionConfig::builder()
        .model_source(ModelSource::huggingface(&model_id))
        .model_type(DiffusionModelType::FluxOffloaded)
        .build();

    // Load the diffusion model
    let model = MistralRsDiffusionModel::new(config).await?;

    println!("Diffusion model loaded successfully!");
    println!();

    // Default generation parameters
    let default_params = DiffusionParams::new().with_size(width, height);

    // Example prompts to generate
    let prompts = vec![
        (
            "landscape",
            "A breathtaking mountain landscape at sunset, with snow-capped peaks \
             reflecting golden light, a crystal clear lake in the foreground, \
             and dramatic clouds painted in shades of orange and purple",
        ),
        (
            "portrait",
            "A photorealistic portrait of a wise elderly person with kind eyes, \
             soft natural lighting, detailed skin texture, wearing simple clothing",
        ),
        (
            "scifi",
            "A futuristic cyberpunk cityscape at night, neon lights reflecting \
             on wet streets, flying vehicles, towering skyscrapers with holographic \
             advertisements, atmospheric fog",
        ),
        (
            "nature",
            "A serene Japanese garden in autumn, with a traditional wooden bridge \
             over a koi pond, maple trees with vibrant red and orange leaves, \
             soft morning mist",
        ),
    ];

    println!("Generating {} example images...", prompts.len());
    println!();

    for (name, prompt) in &prompts {
        println!("Generating: {}", name);
        println!("Prompt: {}", prompt);
        println!("Please wait...");

        let start = std::time::Instant::now();

        match model.generate_image(prompt, default_params.clone()).await {
            Ok(image) => {
                let elapsed = start.elapsed();
                let output_path = if let Some(path) = &image.file_path {
                    // Copy to our output directory with a better name
                    let dest = output_dir.join(format!("{}.png", name));
                    if let Err(e) = std::fs::copy(path, &dest) {
                        println!("Warning: Could not copy image: {}", e);
                        PathBuf::from(path)
                    } else {
                        dest
                    }
                } else {
                    output_dir.join(format!("{}.png", name))
                };

                println!("✓ Image saved to: {}", output_path.display());
                println!("  Size: {}x{}", image.width, image.height);
                println!("  Generation time: {:.2}s", elapsed.as_secs_f64());
            }
            Err(e) => {
                println!("✗ Error generating image: {}", e);
            }
        }
        println!();
    }

    // Summary
    println!("Summary");
    println!("-------");
    println!("Generated images in: {}", output_dir.display());
    println!();

    // Interactive mode
    println!("Interactive Mode");
    println!("----------------");
    println!("Enter a prompt to generate an image (or 'quit' to exit):");
    println!("Tips for good prompts:");
    println!("  - Be specific and descriptive");
    println!("  - Include style keywords (photorealistic, artistic, etc.)");
    println!("  - Mention lighting, mood, and atmosphere");
    println!();

    let mut rl = rustyline::DefaultEditor::new()?;
    let mut image_count = 0;

    loop {
        let readline = rl.readline("Prompt > ");
        match readline {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if line == "quit" || line == "exit" || line == "q" {
                    println!("Goodbye!");
                    break;
                }

                // Handle special commands
                if let Some(stripped) = line.strip_prefix("/size ") {
                    let parts: Vec<&str> = stripped.split('x').collect();
                    if parts.len() == 2 {
                        if let (Ok(w), Ok(h)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
                            println!("Image size set to {}x{}", w, h);
                            println!("(Note: This only affects the next generation)");
                        } else {
                            println!("Invalid size format. Use: /size 1024x1024");
                        }
                    } else {
                        println!("Usage: /size 1024x1024");
                    }
                    continue;
                }

                if line == "/help" {
                    println!("Commands:");
                    println!("  /size WxH  - Set image size (e.g., /size 512x512)");
                    println!("  /help      - Show this help");
                    println!("  quit       - Exit the program");
                    println!();
                    continue;
                }

                println!("Generating image...");
                let start = std::time::Instant::now();

                match model.generate_image(line, default_params.clone()).await {
                    Ok(image) => {
                        let elapsed = start.elapsed();
                        image_count += 1;
                        let filename = format!("generated_{}.png", image_count);
                        let output_path = output_dir.join(&filename);

                        if let Some(path) = &image.file_path {
                            if let Err(e) = std::fs::copy(path, &output_path) {
                                println!("Warning: Could not copy image: {}", e);
                                println!("Original location: {}", path);
                            } else {
                                println!("✓ Saved to: {}", output_path.display());
                            }
                        } else {
                            println!("✓ Generated (no file path returned)");
                        }
                        println!("  Size: {}x{}", image.width, image.height);
                        println!("  Time: {:.2}s", elapsed.as_secs_f64());
                    }
                    Err(e) => {
                        println!("Error: {}", e);
                    }
                }
                println!();
            }
            Err(rustyline::error::ReadlineError::Interrupted) => {
                println!("Interrupted");
                break;
            }
            Err(rustyline::error::ReadlineError::Eof) => {
                println!("EOF");
                break;
            }
            Err(err) => {
                eprintln!("Error: {}", err);
                break;
            }
        }
    }

    Ok(())
}
