use display_error_chain::DisplayErrorChain;
use gemini_rust::{Gemini, GenerationConfig};
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

async fn do_main() -> Result<(), Box<dyn std::error::Error>> {
    // Get API key from environment variable
    let api_key = env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY environment variable not set");
    // Using custom base URL
    let custom_base_url = "https://generativelanguage.googleapis.com/v1beta/";
    let client_custom = Gemini::with_model_and_base_url(
        api_key,
        "models/gemini-2.5-flash-lite-preview-06-17".to_string(),
        custom_base_url.to_string().parse().unwrap(),
    )
    .expect("unable to create Gemini API client");

    info!(
        base_url = custom_base_url,
        "custom base url client created successfully"
    );

    let response = client_custom
        .generate_content()
        .with_system_prompt("You are a helpful assistant.")
        .with_user_message("Hello, can you tell me a joke about programming?")
        .with_generation_config(GenerationConfig {
            temperature: Some(0.7),
            max_output_tokens: Some(100),
            ..Default::default()
        })
        .execute()
        .await?;

    info!(
        response = response.text(),
        "response received from custom base url"
    );

    Ok(())
}
