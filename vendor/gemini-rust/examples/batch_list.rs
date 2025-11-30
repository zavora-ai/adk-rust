//! Batch list example
//!
//! This example demonstrates how to list batch operations using a stream.
//!
//! To run this example, you need to have a Gemini API key.
//!
//! ```sh
//! export GEMINI_API_KEY=your_api_key
//! cargo run --package gemini-rust --example batch_list
//! ```

use display_error_chain::DisplayErrorChain;
use futures::StreamExt;
use gemini_rust::Gemini;
use std::process::ExitCode;
use tracing::{error, info};

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
    // Get the API key from the environment
    let api_key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY not set");

    // Create a new Gemini client
    let gemini = Gemini::new(api_key).expect("unable to create Gemini API client");

    info!("listing all batch operations");

    // List all batch operations using the stream
    let stream = gemini.list_batches(5); // page_size of 5
    tokio::pin!(stream);

    while let Some(result) = stream.next().await {
        match result {
            Ok(operation) => {
                info!(
                    batch_name = operation.name,
                    state = ?operation.metadata.state,
                    created = %operation.metadata.create_time,
                    "batch operation found"
                );
            }
            Err(e) => {
                error!(error = ?e, "error fetching batch operation");
            }
        }
    }

    info!("finished listing batch operations");

    Ok(())
}
