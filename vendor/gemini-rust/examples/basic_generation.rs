use display_error_chain::DisplayErrorChain;
use gemini_rust::Gemini;
use std::env;
use std::process::ExitCode;
use tracing::info;

/// Basic content generation example - demonstrates the simplest usage of the Gemini API
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

    // Create a Gemini client with default settings (Gemini 2.5 Flash)
    let client = Gemini::new(api_key)?;

    info!("basic content generation example starting");

    // Example 1: Simple user message
    let response = client
        .generate_content()
        .with_user_message("Hello, how are you?")
        .execute()
        .await?;

    info!(response = response.text(), "simple response received");

    // Example 2: With system prompt for context
    let response_with_system = client
        .generate_content()
        .with_system_prompt("You are a helpful assistant specializing in Rust programming.")
        .with_user_message("What makes Rust a good choice for systems programming?")
        .execute()
        .await?;

    info!(
        response = response_with_system.text(),
        "response with system prompt received"
    );

    // Example 3: Multiple messages (conversation)
    let conversation_response = client
        .generate_content()
        .with_user_message("I'm learning to code.")
        .with_model_message("That's great! What programming language are you interested in?")
        .with_user_message("I want to learn Rust. Where should I start?")
        .execute()
        .await?;

    info!(
        response = conversation_response.text(),
        "conversation response received"
    );

    info!("\n✅ Basic content generation examples completed successfully!");
    Ok(())
}
