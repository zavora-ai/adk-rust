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

    // Create client
    let client = Gemini::new(api_key).expect("unable to create Gemini API client");

    // Using the full generation config
    info!("starting generation config example with full config");
    let response1 = client
        .generate_content()
        .with_system_prompt("You are a helpful assistant.")
        .with_user_message("Write a short poem about Rust programming language.")
        .with_generation_config(GenerationConfig {
            temperature: Some(0.9),
            top_p: Some(0.8),
            top_k: Some(20),
            max_output_tokens: Some(200),
            candidate_count: Some(1),
            stop_sequences: Some(vec!["END".to_string()]),
            response_mime_type: None,
            response_schema: None,
            thinking_config: None,
            ..Default::default()
        })
        .execute()
        .await?;

    info!(
        temperature = 0.9,
        response = response1.text(),
        "response with high temperature received"
    );

    // Using individual generation parameters
    info!("starting generation config example with individual parameters");
    let response2 = client
        .generate_content()
        .with_system_prompt("You are a helpful assistant.")
        .with_user_message("Write a short poem about Rust programming language.")
        .with_temperature(0.2)
        .with_max_output_tokens(100)
        .execute()
        .await?;

    info!(
        temperature = 0.2,
        response = response2.text(),
        "response with low temperature received"
    );

    // Setting multiple parameters individually
    info!("starting generation config example with multiple individual parameters");
    let response3 = client
        .generate_content()
        .with_system_prompt("You are a helpful assistant.")
        .with_user_message("List 3 benefits of using Rust.")
        .with_temperature(0.7)
        .with_top_p(0.9)
        .with_max_output_tokens(150)
        .with_stop_sequences(vec!["4.".to_string()])
        .execute()
        .await?;

    info!(
        temperature = 0.7,
        top_p = 0.9,
        max_tokens = 150,
        response = response3.text(),
        "response with custom parameters and stop sequence received"
    );

    Ok(())
}
