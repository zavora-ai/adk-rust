use display_error_chain::DisplayErrorChain;
use gemini_rust::{Gemini, Model};
use std::env;
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

#[allow(unused)]
async fn do_main() -> Result<(), Box<dyn std::error::Error>> {
    // Get API key from environment variable
    let api_key = env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY environment variable not set");

    info!("demonstrating different model configuration options");

    // 1. Default Gemini 2.5 Flash model
    let client_default = Gemini::new(api_key.clone())?;
    info!("created client with default Gemini 2.5 Flash model");

    // 2. Gemini 2.5 Pro for advanced tasks (convenience method)
    let client_pro = Gemini::pro(api_key.clone())?;
    info!("created client with Gemini 2.5 Pro model using convenience method");

    // 3. Using enum variants for predefined models
    let client_flash_lite = Gemini::with_model(api_key.clone(), Model::Gemini25FlashLite)?;
    info!("created client with Gemini 2.5 Flash Lite using Model enum");

    let client_embedding = Gemini::with_model(api_key.clone(), Model::TextEmbedding004)?;
    info!("created client with Text Embedding 004 model using Model enum");

    // 4. Using custom model strings for specific versions or preview models
    let client_custom_string = Gemini::with_model(
        api_key.clone(),
        "models/gemini-2.5-flash-image-preview".to_string(),
    )?;
    info!("created client with custom model string for image generation");

    // 5. Using Model::Custom for any other model
    let client_custom_enum = Gemini::with_model(
        api_key.clone(),
        Model::Custom("models/gemini-2.5-flash-preview-tts".to_string()),
    )?;
    info!("created client with Model::Custom for text-to-speech model");

    // Test with the default model
    let test_message = "Hello! Can you tell me which model you are?";

    let response = client_default
        .generate_content()
        .with_user_message(test_message)
        .execute()
        .await?;

    info!(
        model = "default (Gemini 2.5 Flash)",
        response = response.text(),
        "received response from default model"
    );

    // Test with Pro model for comparison
    let response_pro = client_pro
        .generate_content()
        .with_user_message(test_message)
        .execute()
        .await?;

    info!(
        model = "Gemini 2.5 Pro",
        response = response_pro.text(),
        "received response from Pro model"
    );

    info!("✅ Successfully demonstrated all model configuration options!");
    info!("Default model response: {}", response.text());
    info!("Pro model response: {}", response_pro.text());

    Ok(())
}
