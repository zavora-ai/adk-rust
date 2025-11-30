use display_error_chain::DisplayErrorChain;
use gemini_rust::{GeminiBuilder, Model};
use reqwest::ClientBuilder;
use std::env;
use std::process::ExitCode;
use std::time::Duration;
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

    // Configure a custom HTTP client with advanced settings
    let http_client_builder = ClientBuilder::new()
        .timeout(Duration::from_secs(30)) // 30 second timeout
        .user_agent("gemini-rust-example/1.0") // Custom user agent
        .connect_timeout(Duration::from_secs(10)) // Connection timeout
        .pool_idle_timeout(Duration::from_secs(30)) // Connection pool timeout
        .pool_max_idle_per_host(10); // Max idle connections per host
                                     // Uncomment the line below to use a proxy
                                     // .proxy(reqwest::Proxy::http("http://proxy.example.com:8080")?);

    info!("custom HTTP client configured with timeouts and connection pooling");

    // Create Gemini client using the custom HTTP client and builder pattern
    let client = GeminiBuilder::new(api_key)
        .with_model(Model::Gemini25Flash)
        .with_http_client(http_client_builder)
        .with_base_url("https://generativelanguage.googleapis.com/v1beta/".parse()?)
        .build()?;

    info!("Gemini client created with custom HTTP configuration");

    // Make a request to test the configuration
    let response = client
        .generate_content()
        .with_system_prompt("You are a helpful assistant that provides concise answers.")
        .with_user_message(
            "Explain the benefits of using custom HTTP client configuration in web applications.",
        )
        .execute()
        .await?;

    info!(
        response = response.text(),
        "response received using custom HTTP client"
    );

    Ok(())
}
