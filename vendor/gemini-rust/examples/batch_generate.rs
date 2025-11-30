//! Batch generate content example
//!
//! This example demonstrates how to use the synchronous batch generate content API to generate content for multiple requests at once.
//!
//! To run this example, you need to have a Gemini API key. You can get one from the Google AI Studio.
//!
//! Once you have the API key, you can run this example by setting the `GEMINI_API_KEY` environment variable:
//!
//! ```sh
//! export GEMINI_API_KEY=your_api_key
//! cargo run --package gemini-rust --example batch_generate
//! ```

use display_error_chain::DisplayErrorChain;
use gemini_rust::{Batch, BatchHandleError, BatchStatus, Gemini, Message};
use std::process::ExitCode;
use std::time::Duration;
use tracing::{error, info, warn};

/// Waits for the batch operation to complete by periodically polling its status.
///
/// This method polls the batch status with a specified delay until the operation
/// reaches a terminal state (Succeeded, Failed, Cancelled, or Expired).
///
/// Consumes the batch and returns the final status. If there's an error during polling,
/// the batch is returned in the error variant so it can be retried.
pub async fn wait_for_completion(
    batch: Batch,
    delay: Duration,
) -> Result<BatchStatus, (Batch, BatchHandleError)> {
    let batch_name = batch.name.clone();
    loop {
        match batch.status().await {
            Ok(status) => match status {
                BatchStatus::Succeeded { .. } | BatchStatus::Cancelled => return Ok(status),
                BatchStatus::Expired => {
                    return Err((batch, BatchHandleError::BatchExpired { name: batch_name }));
                }
                _ => tokio::time::sleep(delay).await,
            },
            Err(e) => match e {
                BatchHandleError::BatchFailed { .. } => return Err((batch, e)),
                _ => return Err((batch, e)), // Return the batch and error for retry
            },
        }
    }
}

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

    // Create the first request
    let request1 = gemini
        .generate_content()
        .with_message(Message::user("What is the meaning of life?"))
        .build();

    // Create the second request
    let request2 = gemini
        .generate_content()
        .with_message(Message::user("What is the best programming language?"))
        .build();

    // Create the batch request
    let batch = gemini
        .batch_generate_content()
        .with_request(request1)
        .with_request(request2)
        .execute()
        .await?;

    // Print the batch information
    info!(batch_name = batch.name(), "batch created successfully");

    // Wait for the batch to complete
    info!("waiting for batch to complete");
    match wait_for_completion(batch, Duration::from_secs(5)).await {
        Ok(final_status) => {
            // Print the final status
            match final_status {
                BatchStatus::Succeeded { results } => {
                    info!("batch succeeded");
                    for item in results {
                        match item.response {
                            Ok(response) => {
                                info!(
                                    key = item.meta.key,
                                    response = response.text(),
                                    "batch response"
                                );
                            }
                            Err(error) => {
                                error!(
                                    key = item.meta.key,
                                    code = error.code,
                                    message = error.message,
                                    "batch error"
                                );
                                if let Some(details) = &error.details {
                                    error!(details = ?details, "error details");
                                }
                            }
                        }
                    }
                }
                BatchStatus::Cancelled => {
                    warn!("batch was cancelled");
                }
                BatchStatus::Expired => {
                    warn!("batch expired");
                }
                _ => {
                    warn!(status = ?final_status, "batch finished with unexpected status");
                }
            }
        }
        Err((_batch, e)) => {
            error!(error = %e, "batch failed - you can retry with the returned batch");
            // Here you could retry: batch.wait_for_completion(Duration::from_secs(5)).await, etc.
        }
    }

    Ok(())
}
