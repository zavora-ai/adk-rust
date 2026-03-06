//! Gemini embedding provider using the `adk-gemini` crate.
//!
//! This module is only available when the `gemini` feature is enabled.

use async_trait::async_trait;
use tracing::{debug, error};

use adk_gemini::{EmbedBuilder, Gemini, Model, TaskType};

use crate::embedding::EmbeddingProvider;
use crate::error::{RagError, Result};

/// An [`EmbeddingProvider`] backed by the Gemini embedding API.
///
/// Wraps an [`adk_gemini::Gemini`] client and delegates to its
/// [`EmbedBuilder`] for single and batch embedding requests.
///
/// # Configuration
///
/// - `task_type` – defaults to [`TaskType::RetrievalDocument`] for ingestion
///   and [`TaskType::RetrievalQuery`] for queries. Override with
///   [`GeminiEmbeddingProvider::with_task_type`].
/// - `output_dimensionality` – optional truncation of the output vector.
///
/// # Example
///
/// ```rust,ignore
/// use adk_rag::gemini::GeminiEmbeddingProvider;
///
/// let provider = GeminiEmbeddingProvider::new("your-api-key")?;
/// let embedding = provider.embed("hello world").await?;
/// ```
pub struct GeminiEmbeddingProvider {
    client: Gemini,
    task_type: TaskType,
    output_dimensionality: Option<i32>,
    dimensions: usize,
}

impl GeminiEmbeddingProvider {
    /// Default embedding dimensions for `gemini-embedding-001`.
    const DEFAULT_DIMENSIONS: usize = 3072;

    /// Create a new provider using the given API key and the default
    /// `gemini-embedding-001` model.
    pub fn new(api_key: impl AsRef<str>) -> Result<Self> {
        let client = Gemini::with_model(api_key, Model::GeminiEmbedding001).map_err(|e| {
            RagError::EmbeddingError {
                provider: "Gemini".to_string(),
                message: format!("failed to create Gemini client: {e}"),
            }
        })?;

        Ok(Self {
            client,
            task_type: TaskType::RetrievalDocument,
            output_dimensionality: None,
            dimensions: Self::DEFAULT_DIMENSIONS,
        })
    }

    /// Create a new provider from an existing [`Gemini`] client.
    ///
    /// Use this when you need full control over the client configuration
    /// (e.g. Vertex AI, custom base URL).
    pub fn from_client(client: Gemini) -> Self {
        Self {
            client,
            task_type: TaskType::RetrievalDocument,
            output_dimensionality: None,
            dimensions: Self::DEFAULT_DIMENSIONS,
        }
    }

    /// Set the task type used for embedding requests.
    pub fn with_task_type(mut self, task_type: TaskType) -> Self {
        self.task_type = task_type;
        self
    }

    /// Set the output dimensionality (truncates the embedding vector).
    pub fn with_output_dimensionality(mut self, dims: i32) -> Self {
        self.output_dimensionality = Some(dims);
        self.dimensions = dims as usize;
        self
    }

    /// Build an [`EmbedBuilder`] pre-configured with this provider's settings.
    fn embed_builder(&self) -> EmbedBuilder {
        let mut builder = self.client.embed_content().with_task_type(self.task_type.clone());

        if let Some(dims) = self.output_dimensionality {
            builder = builder.with_output_dimensionality(dims);
        }

        builder
    }
}

#[async_trait]
impl EmbeddingProvider for GeminiEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        debug!(provider = "Gemini", text_len = text.len(), "embedding single text");

        let response = self.embed_builder().with_text(text).execute().await.map_err(|e| {
            error!(provider = "Gemini", error = %e, "embedding request failed");
            RagError::EmbeddingError { provider: "Gemini".to_string(), message: format!("{e}") }
        })?;

        Ok(response.embedding.values)
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        debug!(provider = "Gemini", batch_size = texts.len(), "embedding batch");

        let response = self
            .embed_builder()
            .with_chunks(texts.iter().map(|t| t.to_string()).collect())
            .execute_batch()
            .await
            .map_err(|e| {
                error!(provider = "Gemini", error = %e, "batch embedding request failed");
                RagError::EmbeddingError { provider: "Gemini".to_string(), message: format!("{e}") }
            })?;

        Ok(response.embeddings.into_iter().map(|e| e.values).collect())
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}
