use serde::{Deserialize, Serialize};
use snafu::Snafu;
use time::OffsetDateTime;

use crate::Model;
use crate::common::serde::*;
use crate::generation::{GenerateContentRequest, GenerationResponse};

/// Validation error for batch requests
#[derive(Debug, Snafu)]
pub enum ValidationError {
    /// A request in the batch is invalid
    #[snafu(display("invalid batch request: {message}"))]
    InvalidRequest {
        /// Description of the validation failure
        message: String,
    },
}

/// Batch file request line JSON representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchRequestFileItem {
    /// Batch generation request (wrapped in request field for API compatibility)
    pub request: GenerateContentRequest,
    /// Batch request unique identifier
    #[serde(with = "key_as_string")]
    pub key: usize,
}

/// Batch file response line JSON representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchResponseFileItem {
    /// Batch response (wrapped in response field for API compatibility)
    #[serde(flatten)]
    pub response: BatchGenerateContentResponseItem,
    /// Batch response unique identifier
    #[serde(with = "key_as_string")]
    pub key: usize,
}

impl From<BatchGenerateContentResponseItem> for Result<GenerationResponse, IndividualRequestError> {
    fn from(response: BatchGenerateContentResponseItem) -> Self {
        match response {
            BatchGenerateContentResponseItem::Response(r) => Ok(r),
            BatchGenerateContentResponseItem::Error(err) => Err(err),
        }
    }
}

/// Represents the response of a batch operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BatchOperationResponse {
    /// Response with inlined responses
    #[serde(rename_all = "camelCase")]
    InlinedResponses { inlined_responses: InlinedResponses },
    /// Response with a file containing results
    #[serde(rename_all = "camelCase")]
    ResponsesFile { responses_file: String },
}

/// A container for inlined responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InlinedResponses {
    /// The list of batch response items
    pub inlined_responses: Vec<InlinedBatchGenerationResponseItem>,
}

/// Represents a single response item within an inlined batch response.
///
/// This structure combines request metadata with the actual response or error,
/// used when batch results are returned inline rather than in a separate file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InlinedBatchGenerationResponseItem {
    /// Request metadata containing the original key and other identifiers
    pub metadata: RequestMetadata,
    /// The actual response content or error for this batch item
    #[serde(flatten)]
    pub result: BatchGenerateContentResponseItem,
}

/// An item in a batch generate content response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BatchGenerateContentResponseItem {
    /// Successful response item
    Response(GenerationResponse),
    /// Error response item
    Error(IndividualRequestError),
}

/// An error for an individual request in a batch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IndividualRequestError {
    /// The error code
    pub code: i32,
    /// The error message
    pub message: String,
    /// Additional details about the error
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

/// Response from the Gemini API for batch content generation (async batch creation)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchGenerateContentResponse {
    /// The name/ID of the created batch
    pub name: String,
    /// Metadata about the batch
    pub metadata: BatchMetadata,
}

/// Metadata for the batch operation
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchMetadata {
    /// Type annotation
    #[serde(rename = "@type")]
    pub type_annotation: String,
    /// Model used for the batch
    pub model: Model,
    /// Display name of the batch
    pub display_name: String,
    /// Creation time
    #[serde(with = "time::serde::rfc3339")]
    pub create_time: OffsetDateTime,
    /// Update time
    #[serde(with = "time::serde::rfc3339")]
    pub update_time: OffsetDateTime,
    /// Batch statistics
    pub batch_stats: BatchStats,
    /// Current state of the batch
    pub state: BatchState,
    /// Name of the batch (duplicate)
    pub name: String,
}

/// Individual batch request item
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchRequestItem {
    /// The actual request
    pub request: GenerateContentRequest,
    /// Metadata for the request
    pub metadata: RequestMetadata,
}

/// Request for batch content generation (corrected format)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchGenerateContentRequest {
    /// The batch configuration
    pub batch: BatchConfig,
}

impl BatchGenerateContentRequest {
    /// Validate the batch request
    pub fn validate(&self) -> Result<(), ValidationError> {
        self.batch.validate()
    }
}

/// Batch configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchConfig {
    /// Display name for the batch
    pub display_name: String,
    /// Input configuration
    pub input_config: InputConfig,
}

impl BatchConfig {
    /// Validate the batch configuration
    pub fn validate(&self) -> Result<(), ValidationError> {
        match &self.input_config {
            InputConfig::Requests(container) => {
                if container.requests.is_empty() {
                    return Err(ValidationError::InvalidRequest {
                        message: "batch must contain at least one request".to_string(),
                    });
                }
            }
            InputConfig::FileName(_) => {}
        }
        Ok(())
    }
}

/// The state of a batch operation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[allow(clippy::enum_variant_names)]
pub enum BatchState {
    /// State is unspecified
    BatchStateUnspecified,
    /// Batch is pending execution
    BatchStatePending,
    /// Batch is currently running
    BatchStateRunning,
    /// Batch completed successfully
    BatchStateSucceeded,
    /// Batch failed during execution
    BatchStateFailed,
    /// Batch was cancelled
    BatchStateCancelled,
    /// Batch expired before completion
    BatchStateExpired,
}

/// Statistics for the batch
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchStats {
    /// Total number of requests in the batch
    #[serde(with = "i64_as_string")]
    pub request_count: i64,
    /// Number of pending requests
    #[serde(default, with = "i64_as_string::optional")]
    pub pending_request_count: Option<i64>,
    /// Number of completed requests
    #[serde(default, with = "i64_as_string::optional")]
    pub completed_request_count: Option<i64>,
    /// Number of failed requests
    #[serde(default, with = "i64_as_string::optional")]
    pub failed_request_count: Option<i64>,
    /// Number of successful requests
    #[serde(default, with = "i64_as_string::optional")]
    pub successful_request_count: Option<i64>,
}

/// Represents a long-running operation from the Gemini API.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchOperation {
    /// The resource name of the operation
    pub name: String,
    /// Metadata about the batch operation
    pub metadata: BatchMetadata,
    /// Whether the operation is complete
    #[serde(default)]
    pub done: bool,
    /// The result of the operation (if complete)
    #[serde(flatten)]
    pub result: Option<OperationResult>,
}

/// Represents an error within a long-running operation.
#[derive(Debug, Snafu, serde::Deserialize, serde::Serialize)]
pub struct OperationError {
    /// The error code
    pub code: i32,
    /// The error message
    pub message: String,
    // details are not included as they are not consistently typed in the API
}

/// Represents the result of a completed batch operation, which is either a response or an error.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OperationResult {
    /// Successful operation result
    Response(BatchOperationResponse),
    /// Failed operation result
    Error(OperationError),
}

impl From<OperationResult> for Result<BatchOperationResponse, OperationError> {
    fn from(operation: OperationResult) -> Self {
        match operation {
            OperationResult::Response(response) => Ok(response),
            OperationResult::Error(error) => Err(error),
        }
    }
}

/// The outcome of a single request in a batch operation.
/// Response from the Gemini API for listing batch operations.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListBatchesResponse {
    /// A list of batch operations.
    pub operations: Vec<BatchOperation>,
    /// A token to retrieve the next page of results.
    pub next_page_token: Option<String>,
}

/// Input configuration for batch requests
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InputConfig {
    /// The requests to be processed in the batch.
    Requests(RequestsContainer),
    /// The name of the File containing the input requests.
    FileName(String),
}

impl InputConfig {
    /// Returns the batch size of the input configuration.
    ///
    /// Returns `None` if the input configuration is a file name.
    pub fn batch_size(&self) -> Option<usize> {
        match self {
            InputConfig::Requests(container) => Some(container.requests.len()),
            InputConfig::FileName(_) => None,
        }
    }
}

/// Container for requests
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestsContainer {
    /// List of requests
    pub requests: Vec<BatchRequestItem>,
}

/// Metadata for batch request
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestMetadata {
    /// Key for the request
    #[serde(with = "key_as_string")]
    pub key: usize,
}
