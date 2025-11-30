use display_error_chain::DisplayErrorChain;
use gemini_rust::Gemini;
use std::env;
use std::process::ExitCode;
use tracing::{debug, error, info, warn};

/// Comprehensive tracing and telemetry example demonstrating observability features
#[tokio::main]
async fn main() -> ExitCode {
    // Initialize tracing once at startup
    // Use environment variables to control output format:
    // - RUST_LOG_FORMAT=json for structured logging
    // - RUST_LOG=debug for detailed logging
    let format = env::var("RUST_LOG_FORMAT").unwrap_or_default();

    if format == "json" {
        // Structured logging for production
        tracing_subscriber::fmt()
            .with_target(true)
            .with_thread_ids(true)
            .with_level(true)
            .with_file(true)
            .with_line_number(true)
            .with_env_filter(
                tracing_subscriber::EnvFilter::builder()
                    .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
                    .from_env_lossy(),
            )
            .init();
    } else {
        // Human-readable console logging (default)
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::builder()
                    .with_default_directive(tracing::level_filters::LevelFilter::INFO.into())
                    .from_env_lossy(),
            )
            .init();
    }

    info!("🔍 Tracing and Telemetry Example");
    info!("💡 Set RUST_LOG_FORMAT=json for structured output");
    info!("💡 Set RUST_LOG=debug for detailed logging");

    // Run all examples with the same subscriber
    match run_all_examples().await {
        Ok(()) => {
            info!("\n✅ All tracing examples completed successfully");
            ExitCode::SUCCESS
        }
        Err(e) => {
            let error_chain = DisplayErrorChain::new(e.as_ref());
            error!(error.debug = ?e, error.chained = %error_chain, "examples failed");
            info!("\n❌ Examples failed - check logs for details");
            ExitCode::FAILURE
        }
    }
}

/// Run all API examples demonstrating different tracing scenarios
async fn run_all_examples() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY environment variable not set");
    let client = Gemini::new(api_key)?;

    // Example 1: Basic API calls with automatic tracing
    info!("🔍 Example 1: Basic content generation");
    info!("starting basic API calls with tracing");

    let response = client
        .generate_content()
        .with_user_message("Hello! Can you explain what observability means in software?")
        .execute()
        .await?;

    info!(
        response_length = response.text().len(),
        "basic generation completed"
    );
    info!("✅ Basic generation completed");

    // Example 2: Production-ready API calls with comprehensive tracing
    info!("\n🔍 Example 2: Production-style API calls with system prompt");

    info!(
        environment = "production",
        service = "gemini-rust-example",
        "starting production API calls"
    );

    let response = client
        .generate_content()
        .with_system_prompt(
            "You are a helpful assistant that provides concise technical explanations.",
        )
        .with_user_message("What are the key benefits of structured logging in microservices?")
        .execute()
        .await?;

    info!(
        response_length = response.text().len(),
        tokens.estimated = response.text().split_whitespace().count(),
        operation = "content_generation",
        "production API call completed successfully"
    );
    info!("✅ Production-style API call completed");

    // Example 3: Demonstrate different log levels
    info!("\n🔍 Example 3: Different log levels demonstration");

    debug!("this debug message will only show if RUST_LOG=debug");
    info!("this info message shows by default");
    warn!("this is a warning message");

    debug!(
        operation = "log_levels_example",
        "starting log levels demonstration"
    );

    let response = client
        .generate_content()
        .with_user_message(
            "Briefly explain the difference between logging levels: debug, info, warn, error",
        )
        .execute()
        .await?;

    debug!(
        response_preview = &response.text()[..response
            .text()
            .char_indices()
            .nth(100)
            .map(|(n, _c)| n)
            .unwrap_or(response.text().len())],
        "response preview"
    );
    info!(
        response_length = response.text().len(),
        "log levels demonstration completed"
    );
    info!("✅ Log levels demonstration completed");

    Ok(())
}
