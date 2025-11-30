//! Batch delete example
//!
//! This example demonstrates how to delete a batch operation after it has completed.
//! Deleting a batch operation removes its metadata from the system but does not cancel
//! a running operation.
//!
//! To run this example, you need to have a Gemini API key and an existing batch operation.
//! You can get an API key from the Google AI Studio.
//!
//! Once you have the API key, you can run this example by setting the `GEMINI_API_KEY`
//! and `BATCH_NAME` environment variables:
//!
//! ```sh
//! export GEMINI_API_KEY=your_api_key
//! export BATCH_NAME=your_batch_name
//! cargo run --package gemini-rust --example batch_delete
//! ```

use display_error_chain::DisplayErrorChain;
use gemini_rust::{BatchStatus, Gemini};
use std::env;
use std::process::ExitCode;
use tracing::{info, warn};

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
    let api_key = env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY not set");

    // Get the batch name from the environment
    let batch_name = env::var("BATCH_NAME").expect("BATCH_NAME not set");

    // Create a new Gemini client
    let gemini = Gemini::new(api_key).expect("unable to create Gemini API client");

    // Get the batch operation
    let batch = gemini.get_batch(&batch_name);

    // Check the batch status
    match batch.status().await {
        Ok(status) => {
            info!(status = ?status, "batch status retrieved");

            // Only delete completed batches (succeeded, failed, cancelled, or expired)
            match status {
                BatchStatus::Succeeded { .. } | BatchStatus::Cancelled | BatchStatus::Expired => {
                    info!("deleting batch operation");
                    // We need to handle the std::result::Result<(), (Batch, Error)> return type
                    match batch.delete().await {
                        Ok(()) => info!("batch deleted successfully"),
                        Err((_batch, e)) => {
                            warn!(error = ?e, "failed to delete batch - you can retry with the returned batch");
                            // Here you could retry: batch.delete().await, etc.
                        }
                    }
                }
                _ => {
                    info!("batch is still running or pending - use cancel() to stop it or wait for completion before deleting");
                }
            }
        }
        Err(e) => warn!(error = ?e, "failed to get batch status"),
    }

    Ok(())
}
