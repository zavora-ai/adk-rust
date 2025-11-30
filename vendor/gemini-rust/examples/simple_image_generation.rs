use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use display_error_chain::DisplayErrorChain;
use gemini_rust::{Gemini, GenerationConfig};
use std::env;
use std::fs;
use std::process::ExitCode;
use tracing::{info, warn};

/// Simple image generation example
/// This demonstrates the basic usage of Gemini's image generation capabilities
#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .init();

    match do_main().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let error_chain = DisplayErrorChain::new(e.as_ref());
            tracing::error!(error.debug = ?e, error.chained = %error_chain, "execution failed");
            ExitCode::FAILURE
        }
    }
}

async fn do_main() -> Result<(), Box<dyn std::error::Error>> {
    // Get API key from environment variable
    let api_key = env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY environment variable not set");

    // Create client with the image generation model
    // Use Gemini 2.5 Flash Image Preview for image generation
    let client = Gemini::with_model(api_key, "models/gemini-2.5-flash-image-preview".to_string())
        .expect("unable to create Gemini API client");

    info!("starting image generation example");

    // Generate an image from text description
    let response = client
        .generate_content()
        .with_user_message(
            "Create a photorealistic image of a cute robot sitting in a garden, \
             surrounded by colorful flowers. The robot should have a friendly \
             expression and be made of polished metal. The lighting should be \
             soft and natural, as if taken during golden hour.",
        )
        .with_generation_config(GenerationConfig {
            temperature: Some(0.8),
            max_output_tokens: Some(8192),
            ..Default::default()
        })
        .execute()
        .await?;

    // Process the response
    let mut images_saved = 0;
    for candidate in response.candidates.iter() {
        if let Some(parts) = &candidate.content.parts {
            for part in parts.iter() {
                match part {
                    gemini_rust::Part::Text { text, .. } => {
                        info!(response = text, "model text response received");
                    }
                    gemini_rust::Part::InlineData { inline_data } => {
                        info!(mime_type = inline_data.mime_type, "image generated");

                        // Decode and save the image
                        match BASE64.decode(&inline_data.data) {
                            Ok(image_bytes) => {
                                images_saved += 1;
                                let filename = format!("robot_garden_{}.png", images_saved);
                                fs::write(&filename, image_bytes)?;
                                info!(filename = filename, "image saved successfully");
                            }
                            Err(e) => {
                                warn!(error = ?e, "failed to decode image");
                            }
                        }
                    }
                    _ => {
                        info!("other content type found in response");
                    }
                }
            }
        }
    }

    if images_saved == 0 {
        warn!("no images were generated - possible reasons: content policy restrictions, API limitations, or model configuration issues");
    } else {
        info!(
            images_count = images_saved,
            "image generation completed successfully"
        );
    }

    Ok(())
}
