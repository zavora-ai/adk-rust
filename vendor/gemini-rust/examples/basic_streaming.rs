use display_error_chain::DisplayErrorChain;
use futures::TryStreamExt;
use gemini_rust::Gemini;
use std::env;
use std::io::{self, Write};
use std::process::ExitCode;
use tracing::info;

/// Simple streaming responses example - demonstrates real-time content streaming
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

    // Create a Gemini client
    let client = Gemini::new(api_key)?;

    info!("streaming responses example starting");

    info!("🔄 Streaming a story about programming...\n");

    // Create a streaming request
    let mut stream = client
        .generate_content()
        .with_user_message(
            "Tell me a short story about a programmer who discovers a magical bug in their code",
        )
        .execute_stream()
        .await?;

    // Process the stream chunks as they arrive
    let mut full_response = String::new();
    while let Some(chunk) = stream.try_next().await? {
        let text = chunk.text();
        info!("{}", text);
        full_response.push_str(&text);

        // Flush stdout to ensure immediate display
        io::stdout().flush()?;
    }

    info!(
        response_length = full_response.len(),
        "streaming response completed"
    );

    // Example 2: Streaming with system prompt
    info!("\n🔄 Streaming technical explanation...\n");

    let mut tech_stream = client
        .generate_content()
        .with_system_prompt(
            "You are a technical expert who explains complex concepts in simple terms.",
        )
        .with_user_message("Explain how async/await works in Rust")
        .execute_stream()
        .await?;

    while let Some(chunk) = tech_stream.try_next().await? {
        info!("{}", chunk.text());
        io::stdout().flush()?;
    }

    info!("\n\n✅ All streaming examples completed successfully!");
    Ok(())
}
