//! The Batch module for managing batch operations.
//!
//! This module provides the [`BatchHandle`] struct, which is a handle to a long-running batch
//! operation on the Gemini API. It allows for checking the status, canceling, and deleting
//! the operation.
//!
//! The status of a batch operation is represented by the [`BatchStatus`] enum, which can be
//! retrieved using the [`BatchHandle::status()`] method. When a batch completes successfully,
//! it transitions to the [`BatchStatus::Succeeded`] state, which contains a vector of
//! [`BatchGenerationResponseItem`].
//!
//! ## Batch Results
//!
//! The [`BatchGenerationResponseItem`] enum represents the outcome of a single request within the batch:
//! - `Success`: Contains the generated `GenerationResponse` and the original request key.
//! - `Error`: Contains an `IndividualRequestError` and the original request key.
//!
//! Results can be delivered in two ways, depending on the size of the batch job:
//! 1.  **Inlined Responses**: For smaller jobs, the results are included directly in the
//!     batch operation's metadata.
//! 2.  **Response File**: For larger jobs (typically >20MB), the results are written to a
//!     file, and the batch metadata will contain a reference to this file. The SDK
//!     handles the downloading and parsing of this file automatically when you call
//!     `status()` on a completed batch.
//!
//! The results are automatically sorted by their original request key (as a number) to ensure
//! a consistent and predictable order.
//!
//! For more information, see the official Google AI documentation:
//! - [Batch Mode Guide](https://ai.google.dev/gemini-api/docs/batch-mode)
//! - [Batch API Reference](https://ai.google.dev/api/batch-mode)
//!
//! # Design Note: Resource Management in Batch Operations
//!
//! The Batch API methods that consume the [`BatchHandle`] struct (`cancel`, `delete`)
//! return `std::result::Result<T, (Self, crate::Error)>` instead of the crate's `Result<T>`.
//! This design follows patterns used in channel libraries (e.g., `std::sync::mpsc::Receiver`)
//! and provides two key benefits:
//!
//! 1. **Resource Safety**: Once a [`BatchHandle`] is consumed by an operation, it cannot be used again,
//!    preventing invalid operations on deleted or canceled batches.
//!
//! 2. **Error Recovery**: If an operation fails due to transient network issues, both the
//!    [`BatchHandle`] and error information are returned, allowing callers to retry the operation.
//!
//! ## Example usage:
//! ```rust,no_run
//! use gemini_rust::{Gemini, Message};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let client = Gemini::new(std::env::var("GEMINI_API_KEY")?)?;
//!     let request = client.generate_content().with_user_message("Why is the sky blue?").build();
//!     let batch = client.batch_generate_content().with_request(request).execute().await?;
//!
//!     match batch.delete().await {
//!         Ok(()) => println!("Batch deleted successfully!"),
//!         Err((batch, error)) => {
//!             println!("Failed to delete batch: {}", error);
//!             // Can retry: batch.delete().await
//!         }
//!     }
//!     Ok(())
//! }
//! ```

use snafu::{OptionExt, ResultExt, Snafu};
use std::{result::Result, sync::Arc};

use super::model::*;
use crate::{
    client::{Error as ClientError, GeminiClient},
    files::handle::FileHandle,
    GenerationResponse,
};

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("batch '{name}' expired before finishing"))]
    BatchExpired {
        /// Batch name.
        name: String,
    },

    #[snafu(display("batch '{name}' failed"))]
    BatchFailed {
        source: OperationError,
        /// Batch name.
        name: String,
    },

    #[snafu(display("client invocation error"))]
    Client { source: Box<ClientError> },

    #[snafu(display("failed to download batch result file '{file_name}'"))]
    FileDownload {
        source: crate::files::Error,
        file_name: String,
    },

    #[snafu(display("failed to decode batch result file content as UTF-8"))]
    FileDecode { source: std::string::FromUtf8Error },

    #[snafu(display("failed to parse line in batch result file"))]
    FileParse {
        source: serde_json::Error,
        line: String,
    },

    /// This error should never occur, as the Google API contract
    /// guarantees that a result will always be provided.
    ///
    /// I put it here anyway to avoid potential panic in case of
    /// Google's dishonesty or GCP internal errors.
    #[snafu(display("batch '{name}' completed but no result provided - API contract violation"))]
    MissingResult {
        /// Batch name.
        name: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct BatchGenerationResponseItem {
    pub response: Result<GenerationResponse, IndividualRequestError>,
    pub meta: RequestMetadata,
}

/// Represents the overall status of a batch operation.
#[derive(Debug, Clone, PartialEq)]
pub enum BatchStatus {
    /// The operation is waiting to be processed.
    Pending,
    /// The operation is currently being processed.
    Running {
        pending_count: i64,
        completed_count: i64,
        failed_count: i64,
        total_count: i64,
    },
    /// The operation has completed successfully.
    Succeeded {
        results: Vec<BatchGenerationResponseItem>,
    },
    /// The operation was cancelled by the user.
    Cancelled,
    /// The operation has expired.
    Expired,
}

impl BatchStatus {
    async fn parse_response_file(
        response_file: crate::files::model::File,
        client: Arc<GeminiClient>,
    ) -> Result<Vec<BatchGenerationResponseItem>, Error> {
        let file = FileHandle::new(client.clone(), response_file);
        let file_content_bytes = file.download().await.context(FileDownloadSnafu {
            file_name: file.name(),
        })?;
        let file_content = String::from_utf8(file_content_bytes).context(FileDecodeSnafu)?;

        let mut results = vec![];
        for line in file_content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let item: BatchResponseFileItem =
                serde_json::from_str(line).context(FileParseSnafu {
                    line: line.to_string(),
                })?;

            results.push(BatchGenerationResponseItem {
                response: item.response.into(),
                meta: RequestMetadata { key: item.key },
            });
        }
        Ok(results)
    }

    async fn process_successful_response(
        response: BatchOperationResponse,
        client: Arc<GeminiClient>,
    ) -> Result<Vec<BatchGenerationResponseItem>, Error> {
        let results = match response {
            BatchOperationResponse::InlinedResponses { inlined_responses } => inlined_responses
                .inlined_responses
                .into_iter()
                .map(|item| BatchGenerationResponseItem {
                    response: item.result.into(),
                    meta: item.metadata,
                })
                .collect(),
            BatchOperationResponse::ResponsesFile { responses_file } => {
                let file = crate::files::model::File {
                    name: responses_file,
                    ..Default::default()
                };
                Self::parse_response_file(file, client).await?
            }
        };
        Ok(results)
    }

    async fn from_operation(
        operation: BatchOperation,
        client: Arc<GeminiClient>,
    ) -> Result<Self, Error> {
        if operation.done {
            // According to Google API documentation, when done=true, result must be present
            let result = operation.result.context(MissingResultSnafu {
                name: operation.name.clone(),
            })?;

            let response = Result::from(result).context(BatchFailedSnafu {
                name: operation.name,
            })?;

            let mut results = Self::process_successful_response(response, client).await?;
            results.sort_by_key(|r| r.meta.key);

            // Handle terminal states based on metadata for edge cases
            match operation.metadata.state {
                BatchState::BatchStateCancelled => Ok(BatchStatus::Cancelled),
                BatchState::BatchStateExpired => Ok(BatchStatus::Expired),
                _ => Ok(BatchStatus::Succeeded { results }),
            }
        } else {
            // The operation is still in progress.
            match operation.metadata.state {
                BatchState::BatchStatePending => Ok(BatchStatus::Pending),
                BatchState::BatchStateRunning => {
                    let total_count = operation.metadata.batch_stats.request_count;
                    let pending_count = operation
                        .metadata
                        .batch_stats
                        .pending_request_count
                        .unwrap_or(total_count);
                    let completed_count = operation
                        .metadata
                        .batch_stats
                        .completed_request_count
                        .unwrap_or(0);
                    let failed_count = operation
                        .metadata
                        .batch_stats
                        .failed_request_count
                        .unwrap_or(0);
                    Ok(BatchStatus::Running {
                        pending_count,
                        completed_count,
                        failed_count,
                        total_count,
                    })
                }
                // For non-running states when done=false, treat as pending
                _ => Ok(BatchStatus::Pending),
            }
        }
    }
}

/// Represents a long-running batch operation, providing methods to manage its lifecycle.
///
/// A `Batch` object is a handle to a batch operation on the Gemini API. It allows you to
/// check the status, cancel the operation, or delete it once it's no longer needed.
pub struct BatchHandle {
    /// The unique resource name of the batch operation, e.g., `operations/batch-xxxxxxxx`.
    pub name: String,
    client: Arc<GeminiClient>,
}

impl BatchHandle {
    /// Creates a new Batch instance.
    pub(crate) fn new(name: String, client: Arc<GeminiClient>) -> Self {
        Self { name, client }
    }

    /// Returns the unique resource name of the batch operation.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Retrieves the current status of the batch operation by making an API call.
    ///
    /// This method provides a snapshot of the batch's state at a single point in time.
    pub async fn status(&self) -> Result<BatchStatus, Error> {
        let operation: BatchOperation = self
            .client
            .get_batch_operation(&self.name)
            .await
            .map_err(Box::new)
            .context(ClientSnafu)?;

        BatchStatus::from_operation(operation, self.client.clone()).await
    }

    /// Sends a request to the API to cancel the batch operation.
    ///
    /// Cancellation is not guaranteed to be instantaneous. The operation may continue to run for
    /// some time after the cancellation request is made.
    ///
    /// Consumes the batch. If cancellation fails, returns the batch and error information
    /// so it can be retried.
    pub async fn cancel(self) -> Result<(), (Self, ClientError)> {
        match self.client.cancel_batch_operation(&self.name).await {
            Ok(()) => Ok(()),
            Err(e) => Err((self, e)),
        }
    }

    /// Deletes the batch operation resource from the server.
    ///
    /// Note: This method indicates that the client is no longer interested in the operation result.
    /// It does not cancel a running operation. To stop a running batch, use the `cancel` method.
    /// This method should typically be used after the batch has completed.
    ///
    /// Consumes the batch. If deletion fails, returns the batch and error information
    /// so it can be retried.
    pub async fn delete(self) -> Result<(), (Self, ClientError)> {
        match self.client.delete_batch_operation(&self.name).await {
            Ok(()) => Ok(()),
            Err(e) => Err((self, e)),
        }
    }
}
