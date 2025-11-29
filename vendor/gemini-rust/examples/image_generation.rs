use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use display_error_chain::DisplayErrorChain;
use gemini_rust::{Gemini, GenerationConfig};
use std::env;
use std::fs;
use std::process::ExitCode;
use tracing::{info, warn};

/// Example of using Gemini API for image generation (text-to-image)
/// This example demonstrates how to generate images using the Gemini 2.5 Flash Image Preview model
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
    let client = Gemini::with_model(api_key, "models/gemini-2.5-flash-image-preview".to_string())
        .expect("unable to create Gemini API client");

    info!("starting text-to-image generation examples");

    // Example 1: Simple text-to-image generation
    let response = client
        .generate_content()
        .with_user_message(
            "Create a picture of a nano banana dish in a fancy restaurant with a Gemini theme. \
             The scene should be photorealistic with elegant lighting and sophisticated presentation."
        )
        .with_generation_config(GenerationConfig {
            temperature: Some(0.7),
            max_output_tokens: Some(8192), // Higher token limit for image output
            ..Default::default()
        })
        .execute()
        .await?;

    // Process the response - look for both text and image outputs
    for (i, candidate) in response.candidates.iter().enumerate() {
        if let Some(parts) = &candidate.content.parts {
            info!(candidate_number = i + 1, "processing candidate");

            for (j, part) in parts.iter().enumerate() {
                match part {
                    gemini_rust::Part::Text { text, .. } => {
                        info!(
                            response_number = j + 1,
                            text = text,
                            "text response received"
                        );
                    }
                    gemini_rust::Part::InlineData { inline_data } => {
                        info!(
                            response_number = j + 1,
                            mime_type = inline_data.mime_type,
                            "image response found"
                        );

                        // Decode base64 image data and save to file
                        match BASE64.decode(&inline_data.data) {
                            Ok(image_bytes) => {
                                let filename = format!("generated_image_{}.png", j + 1);
                                fs::write(&filename, image_bytes)?;
                                info!(filename = filename, "image saved successfully");
                            }
                            Err(e) => {
                                warn!(error = ?e, "failed to decode image data");
                            }
                        }
                    }
                    _ => {
                        info!("other part type encountered");
                    }
                }
            }
        }
    }

    info!("starting advanced image generation examples");

    // Example 2: Product mockup
    let product_response = client
        .generate_content()
        .with_user_message(
            "A high-resolution, studio-lit product photograph of a minimalist ceramic coffee mug \
             with a matte black finish on a clean white marble surface. The lighting is a \
             three-point softbox setup to eliminate harsh shadows. The camera angle is a \
             slightly elevated 45-degree angle to showcase the mug's elegant handle design. \
             Ultra-realistic, with sharp focus on the mug's texture. 16:9 aspect ratio.",
        )
        .execute()
        .await?;

    save_generated_images(&product_response, "product_mockup")?;

    // Example 3: Logo design with text
    let logo_response = client
        .generate_content()
        .with_user_message(
            "Create a modern, minimalist logo for a coffee shop called 'The Daily Grind' \
             with clean sans-serif typography. The design should feature a stylized coffee bean \
             icon integrated with the text. Use a warm color palette of deep brown and cream. \
             The background must be transparent.",
        )
        .execute()
        .await?;

    save_generated_images(&logo_response, "coffee_logo")?;

    // Example 4: Artistic illustration
    let art_response = client
        .generate_content()
        .with_user_message(
            "A kawaii-style sticker of a happy red panda programmer, featuring round eyes and \
             a cheerful expression, sitting in front of a laptop with tiny hearts floating around. \
             Use a bright and vibrant color palette with soft pastels. The design should have \
             clean line art and cell-shaded style. The background must be transparent.",
        )
        .execute()
        .await?;

    save_generated_images(&art_response, "red_panda_sticker")?;

    // Example 5: Minimalist design
    let minimal_response = client
        .generate_content()
        .with_user_message(
            "A minimalist composition featuring a single, delicate red maple leaf positioned in the \
             bottom-right of the frame. The background is a vast, empty cream canvas, creating \
             significant negative space. Soft, subtle lighting. 16:9 aspect ratio."
        )
        .execute()
        .await?;

    save_generated_images(&minimal_response, "minimalist_leaf")?;

    info!("all image generation examples completed - check current directory for generated files");

    Ok(())
}

/// Helper function to save generated images from a response
fn save_generated_images(
    response: &gemini_rust::GenerationResponse,
    prefix: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    for candidate in response.candidates.iter() {
        if let Some(parts) = &candidate.content.parts {
            let mut image_count = 0;
            let mut text_parts = Vec::new();

            for part in parts.iter() {
                match part {
                    gemini_rust::Part::Text { text, .. } => {
                        text_parts.push(text.clone());
                    }
                    gemini_rust::Part::InlineData { inline_data } => {
                        image_count += 1;
                        match BASE64.decode(&inline_data.data) {
                            Ok(image_bytes) => {
                                let filename = format!("{}_{}.png", prefix, image_count);
                                fs::write(&filename, image_bytes)?;
                                info!(
                                    filename = filename,
                                    prefix = prefix,
                                    "generated image saved"
                                );
                            }
                            Err(e) => {
                                warn!(error = ?e, prefix = prefix, "failed to decode image data");
                            }
                        }
                    }
                    _ => {}
                }
            }

            // Log any text responses
            if !text_parts.is_empty() {
                info!(
                    prefix = prefix,
                    text = text_parts.join("\n"),
                    "text response received"
                );
            }
        }
    }
    Ok(())
}
