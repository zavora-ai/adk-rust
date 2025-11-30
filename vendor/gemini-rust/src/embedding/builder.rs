use std::sync::Arc;
use tracing::instrument;

use super::model::{
    BatchContentEmbeddingResponse, BatchEmbedContentsRequest, ContentEmbeddingResponse,
    EmbedContentRequest, TaskType,
};
use crate::{
    client::{Error as ClientError, GeminiClient},
    Content, Message,
};

/// Builder for embed generation requests
pub struct EmbedBuilder {
    client: Arc<GeminiClient>,
    contents: Vec<Content>,
    task_type: Option<TaskType>,
    title: Option<String>,
    output_dimensionality: Option<i32>,
}

impl EmbedBuilder {
    /// Create a new embed builder
    pub(crate) fn new(client: Arc<GeminiClient>) -> Self {
        Self {
            client,
            contents: Vec::new(),
            task_type: None,
            title: None,
            output_dimensionality: None,
        }
    }

    /// Add a vec of text to embed to the request
    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        let message = Message::embed(text);
        self.contents.push(message.content);
        self
    }

    /// Add a vec of chunks to batch embed to the request
    pub fn with_chunks(mut self, chunks: Vec<impl Into<String>>) -> Self {
        //for each chunks
        for chunk in chunks {
            let message = Message::embed(chunk);
            self.contents.push(message.content);
        }
        self
    }

    /// Specify embedding task type
    pub fn with_task_type(mut self, task_type: TaskType) -> Self {
        self.task_type = Some(task_type);
        self
    }

    /// Specify document title
    /// Supported by newer models since 2024 only !!
    pub fn with_title(mut self, title: String) -> Self {
        self.title = Some(title);
        self
    }

    /// Specify output_dimensionality. If set, excessive values in the output embedding are truncated from the end
    pub fn with_output_dimensionality(mut self, output_dimensionality: i32) -> Self {
        self.output_dimensionality = Some(output_dimensionality);
        self
    }

    /// Execute the request
    #[instrument(skip_all, fields(
        task.type = self.task_type.as_ref().map(AsRef::<str>::as_ref),
        title = self.title,
        output.dimensionality = self.output_dimensionality
    ))]
    pub async fn execute(self) -> Result<ContentEmbeddingResponse, ClientError> {
        let request = EmbedContentRequest {
            model: self.client.model.clone(),
            content: self.contents.first().expect("No content set").clone(),
            task_type: self.task_type,
            title: self.title,
            output_dimensionality: self.output_dimensionality,
        };

        self.client.embed_content(request).await
    }

    /// Execute the request
    #[instrument(skip_all, fields(
        batch.size = self.contents.len(),
        task.type = self.task_type.as_ref().map(AsRef::<str>::as_ref),
        title = self.title,
        output.dimensionality = self.output_dimensionality
    ))]
    pub async fn execute_batch(self) -> Result<BatchContentEmbeddingResponse, ClientError> {
        let mut batch_request = BatchEmbedContentsRequest {
            requests: Vec::new(),
        };

        for content in self.contents {
            let request = EmbedContentRequest {
                model: self.client.model.clone(),
                content: content.clone(),
                task_type: self.task_type.clone(),
                title: self.title.clone(),
                output_dimensionality: self.output_dimensionality,
            };
            batch_request.requests.push(request);
        }

        self.client.embed_content_batch(batch_request).await
    }
}
