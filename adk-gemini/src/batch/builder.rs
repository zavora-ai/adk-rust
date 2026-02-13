use snafu::ResultExt;
use std::sync::Arc;
use tracing::{Span, instrument};

use super::handle::BatchHandle;
use super::model::*;
use super::*;
use crate::{client::GeminiClient, generation::GenerateContentRequest};

/// A builder for creating and executing synchronous batch content generation requests.
///
/// This builder simplifies the process of constructing a batch request, allowing you to
/// add multiple  items and then execute them as a single
/// long-running operation.
pub struct BatchBuilder {
    client: Arc<GeminiClient>,
    display_name: String,
    requests: Vec<GenerateContentRequest>,
}

impl BatchBuilder {
    /// Create a new batch builder
    pub(crate) fn new(client: Arc<GeminiClient>) -> Self {
        Self { client, display_name: "RustBatch".to_string(), requests: Vec::new() }
    }

    /// Sets the user-friendly display name for the batch request.
    pub fn with_name(mut self, name: String) -> Self {
        self.display_name = name;
        self
    }

    /// Sets all requests for the batch operation, replacing any existing requests.
    pub fn with_requests(mut self, requests: Vec<GenerateContentRequest>) -> Self {
        self.requests = requests;
        self
    }

    /// Adds a single  to the batch.
    pub fn with_request(mut self, request: GenerateContentRequest) -> Self {
        self.requests.push(request);
        self
    }

    /// Constructs the final  from the builder's configuration.
    ///
    /// This method consumes the builder.
    pub fn build(self) -> BatchGenerateContentRequest {
        let batch_requests: Vec<BatchRequestItem> = self
            .requests
            .into_iter()
            .enumerate()
            .map(|(key, request)| BatchRequestItem { request, metadata: RequestMetadata { key } })
            .collect();

        BatchGenerateContentRequest {
            batch: BatchConfig {
                display_name: self.display_name,
                input_config: InputConfig::Requests(RequestsContainer { requests: batch_requests }),
            },
        }
    }

    /// Submits the batch request to the Gemini API and returns a  handle.
    ///
    /// This method consumes the builder and initiates the long-running batch operation.
    #[instrument(skip_all, fields(
        batch.display_name = self.display_name,
        batch.size = self.requests.len()
    ))]
    pub async fn execute(self) -> Result<BatchHandle, Error> {
        let client = self.client.clone();
        let request = self.build();
        let response = client.batch_generate_content_raw(request).await.context(ClientSnafu)?;
        Ok(BatchHandle::new(response.name, client))
    }

    /// Executes the batch request by first uploading the requests as a JSON file.
    ///
    /// This method is ideal for large batch jobs that might exceed inline request limits.
    /// It consumes the builder, serializes the requests to the JSON Lines format,
    /// uploads the content as a file, and then starts the batch operation using that file.
    #[instrument(skip_all, fields(
        batch.display_name = self.display_name,
        batch.size = self.requests.len()
    ))]
    pub async fn execute_as_file(self) -> Result<BatchHandle, Error> {
        let mut json_lines = String::new();
        for (index, item) in self.requests.into_iter().enumerate() {
            let item = BatchRequestFileItem { request: item, key: index };

            let line = serde_json::to_string(&item).context(SerializeSnafu)?;
            json_lines.push_str(&line);
            json_lines.push('\n');
        }
        let json_bytes = json_lines.into_bytes();
        Span::current().record("file.size", json_bytes.len());

        let file_display_name = format!("{}-input.jsonl", self.display_name);
        let file = crate::files::builder::FileBuilder::new(self.client.clone(), json_bytes)
            .display_name(file_display_name)
            .with_mime_type(
                "application/jsonl".parse().expect("failed to parse MIME type 'application/jsonl'"),
            )
            .upload()
            .await
            .context(FileSnafu)?;

        let request = BatchGenerateContentRequest {
            batch: BatchConfig {
                display_name: self.display_name,
                input_config: InputConfig::FileName(file.name().to_string()),
            },
        };

        let client = self.client.clone();
        let response = client.batch_generate_content_raw(request).await.context(ClientSnafu)?;

        Ok(BatchHandle::new(response.name, client))
    }
}
