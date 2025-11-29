use base64::{engine::general_purpose, Engine as _};
use display_error_chain::DisplayErrorChain;
use gemini_rust::{Gemini, GenerationConfig};
use std::env;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::ExitCode;
use tracing::info;

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

    // Image file path (in the same directory)
    let image_path = Path::new(file!())
        .parent()
        .unwrap_or(Path::new("."))
        .join("image-example.webp"); // Replace with your image filename

    // Read the image file
    let mut file = File::open(&image_path)?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)?;

    // Convert to base64
    let data = general_purpose::STANDARD.encode(&buffer);

    info!(image_path = ?image_path, file_size = buffer.len(), "image loaded and encoded to base64");

    // Create client
    let client = Gemini::new(api_key).expect("unable to create Gemini API client");

    info!("starting image description request");
    let response = client
        .generate_content()
        .with_inline_data(data, "image/webp")
        .with_response_mime_type("text/plain")
        .with_generation_config(GenerationConfig {
            temperature: Some(0.7),
            max_output_tokens: Some(400),
            ..Default::default()
        })
        .execute()
        .await?;

    info!(response = response.text(), "image description received");

    Ok(())
}
