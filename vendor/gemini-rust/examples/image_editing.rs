use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use display_error_chain::DisplayErrorChain;
use gemini_rust::Gemini;
use std::env;
use std::fs;
use std::process::ExitCode;
use tracing::{info, warn};

/// Image editing example using Gemini API
/// This demonstrates how to edit existing images using text prompts
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

    info!("starting image editing example with Gemini");

    // First, let's generate a base image to edit
    info!("step 1: generating base image");
    let base_response = client
        .generate_content()
        .with_user_message(
            "Create a simple landscape image with a blue sky, green grass, \
             and a single white house in the center. The style should be \
             clean and minimalist.",
        )
        .execute()
        .await?;

    // Save the base image
    let mut base_image_data = None;
    for candidate in base_response.candidates.iter() {
        if let Some(parts) = &candidate.content.parts {
            for part in parts.iter() {
                if let gemini_rust::Part::InlineData { inline_data } = part {
                    base_image_data = Some(inline_data.data.clone());
                    let image_bytes = BASE64.decode(&inline_data.data)?;
                    fs::write("base_landscape.png", image_bytes)?;
                    info!(filename = "base_landscape.png", "base image saved");
                    break;
                }
            }
        }
    }

    let base_data = match base_image_data {
        Some(data) => data,
        None => {
            warn!("failed to generate base image");
            return Ok(());
        }
    };

    info!("step 2: editing the image");

    // Example 1: Add elements to the image
    info!("adding red barn to the scene");
    let edit_response1 = client
        .generate_content()
        .with_user_message(
            "Add a red barn to the left side of this landscape image. \
             The barn should fit naturally into the scene and match \
             the minimalist style. Keep everything else exactly the same.",
        )
        .with_inline_data(&base_data, "image/png")
        .execute()
        .await?;

    save_generated_images(&edit_response1, "landscape_with_barn")?;

    // Example 2: Change the weather/atmosphere
    info!("changing scene to sunset");
    let edit_response2 = client
        .generate_content()
        .with_user_message(
            "Transform this landscape into a beautiful sunset scene. \
             Change the sky to warm orange and pink colors, add a \
             setting sun, and adjust the lighting to match golden hour. \
             Keep the house and grass but make them glow with sunset light.",
        )
        .with_inline_data(&base_data, "image/png")
        .execute()
        .await?;

    save_generated_images(&edit_response2, "sunset_landscape")?;

    // Example 3: Style transfer
    info!("converting to watercolor style");
    let edit_response3 = client
        .generate_content()
        .with_user_message(
            "Transform this landscape into a watercolor painting style. \
             Preserve the composition but render it with soft, flowing \
             watercolor brushstrokes, gentle color bleeding, and the \
             characteristic transparency of watercolor art.",
        )
        .with_inline_data(&base_data, "image/png")
        .execute()
        .await?;

    save_generated_images(&edit_response3, "watercolor_landscape")?;

    info!("image editing examples completed - check generated files: base_landscape.png, landscape_with_barn_*.png, sunset_landscape_*.png, watercolor_landscape_*.png");

    Ok(())
}

/// Helper function to save generated images from a response
fn save_generated_images(
    response: &gemini_rust::GenerationResponse,
    prefix: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut image_count = 0;

    for candidate in response.candidates.iter() {
        if let Some(parts) = &candidate.content.parts {
            for part in parts.iter() {
                match part {
                    gemini_rust::Part::Text { text, .. } => {
                        if !text.trim().is_empty() {
                            info!(text = text.trim(), prefix = prefix, "model text response");
                        }
                    }
                    gemini_rust::Part::InlineData { inline_data } => {
                        image_count += 1;
                        match BASE64.decode(&inline_data.data) {
                            Ok(image_bytes) => {
                                let filename = format!("{}_{}.png", prefix, image_count);
                                fs::write(&filename, image_bytes)?;
                                info!(filename = filename, prefix = prefix, "edited image saved");
                            }
                            Err(e) => {
                                warn!(error = ?e, prefix = prefix, "failed to decode image");
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    if image_count == 0 {
        warn!(prefix = prefix, "no images were generated for this edit");
    }

    Ok(())
}
