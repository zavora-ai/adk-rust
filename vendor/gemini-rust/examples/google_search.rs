use display_error_chain::DisplayErrorChain;
use gemini_rust::{Gemini, Tool};
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

    info!("starting google search tool example");

    // Create a Google Search tool
    let google_search_tool = Tool::google_search();

    // Create a request with Google Search tool
    let response = client
        .generate_content()
        .with_user_message("What is the current Google stock price?")
        .with_tool(google_search_tool)
        .execute()
        .await?;

    info!(
        response = response.text(),
        "google search response received"
    );

    Ok(())
}
