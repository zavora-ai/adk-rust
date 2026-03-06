//! OpenAI embedding provider using the OpenAI embeddings API.
//!
//! This module is only available when the `openai` feature is enabled.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{debug, error};

use crate::embedding::EmbeddingProvider;
use crate::error::{RagError, Result};

/// The default OpenAI embeddings API endpoint.
const OPENAI_EMBEDDINGS_URL: &str = "https://api.openai.com/v1/embeddings";

/// The default model for OpenAI embeddings.
const DEFAULT_MODEL: &str = "text-embedding-3-small";

/// The default dimensionality for `text-embedding-3-small`.
const DEFAULT_DIMENSIONS: usize = 1536;

/// An [`EmbeddingProvider`] backed by the OpenAI embeddings API.
///
/// Uses `reqwest` to call the `/v1/embeddings` endpoint directly.
///
/// # Configuration
///
/// - `model` – defaults to `text-embedding-3-small`.
/// - `dimensions` – optional Matryoshka dimension override.
/// - `api_key` – from the constructor or the `OPENAI_API_KEY` environment variable.
///
/// # Example
///
/// ```rust,ignore
/// use adk_rag::openai::OpenAIEmbeddingProvider;
///
/// let provider = OpenAIEmbeddingProvider::new("sk-...")?;
/// let embedding = provider.embed("hello world").await?;
/// ```
pub struct OpenAIEmbeddingProvider {
    client: reqwest::Client,
    api_key: String,
    model: String,
    dimensions: usize,
    /// If set, passed to the API for Matryoshka dimension truncation.
    request_dimensions: Option<usize>,
}

impl OpenAIEmbeddingProvider {
    /// Create a new provider with the given API key.
    ///
    /// Uses the default model (`text-embedding-3-small`) and dimensions (1536).
    pub fn new(api_key: impl Into<String>) -> Result<Self> {
        let api_key = api_key.into();
        if api_key.is_empty() {
            return Err(RagError::EmbeddingError {
                provider: "OpenAI".to_string(),
                message: "API key must not be empty".to_string(),
            });
        }

        Ok(Self {
            client: reqwest::Client::new(),
            api_key,
            model: DEFAULT_MODEL.into(),
            dimensions: DEFAULT_DIMENSIONS,
            request_dimensions: None,
        })
    }

    /// Create a new provider using the `OPENAI_API_KEY` environment variable.
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| RagError::EmbeddingError {
            provider: "OpenAI".to_string(),
            message: "OPENAI_API_KEY environment variable not set".to_string(),
        })?;
        Self::new(api_key)
    }

    /// Set the model name (e.g. `text-embedding-3-large`).
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Set the output dimensions (Matryoshka support).
    ///
    /// When set, the API returns embeddings truncated to this size.
    /// This also updates the value returned by [`dimensions()`](EmbeddingProvider::dimensions).
    pub fn with_dimensions(mut self, dims: usize) -> Self {
        self.dimensions = dims;
        self.request_dimensions = Some(dims);
        self
    }
}

// ── OpenAI API request/response types ──────────────────────────────

#[derive(Serialize)]
struct EmbeddingRequest<'a> {
    model: &'a str,
    input: Vec<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dimensions: Option<usize>,
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
}

#[derive(Deserialize)]
struct ErrorResponse {
    error: ErrorDetail,
}

#[derive(Deserialize)]
struct ErrorDetail {
    message: String,
}

// ── EmbeddingProvider implementation ───────────────────────────────

#[async_trait]
impl EmbeddingProvider for OpenAIEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        debug!(provider = "OpenAI", text_len = text.len(), "embedding single text");

        let results = self.embed_batch(&[text]).await?;
        results.into_iter().next().ok_or_else(|| RagError::EmbeddingError {
            provider: "OpenAI".to_string(),
            message: "API returned empty response".to_string(),
        })
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        debug!(
            provider = "OpenAI",
            batch_size = texts.len(),
            model = %self.model,
            "embedding batch"
        );

        let request_body = EmbeddingRequest {
            model: &self.model,
            input: texts.to_vec(),
            dimensions: self.request_dimensions,
        };

        let response = self
            .client
            .post(OPENAI_EMBEDDINGS_URL)
            .bearer_auth(&self.api_key)
            .json(&request_body)
            .send()
            .await
            .map_err(|e| {
                error!(provider = "OpenAI", error = %e, "request failed");
                RagError::EmbeddingError {
                    provider: "OpenAI".to_string(),
                    message: format!("request failed: {e}"),
                }
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let detail = serde_json::from_str::<ErrorResponse>(&body)
                .map(|e| e.error.message)
                .unwrap_or(body);

            error!(provider = "OpenAI", %status, "API error");
            return Err(RagError::EmbeddingError {
                provider: "OpenAI".to_string(),
                message: format!("API returned {status}: {detail}"),
            });
        }

        let embedding_response: EmbeddingResponse = response.json().await.map_err(|e| {
            error!(provider = "OpenAI", error = %e, "failed to parse response");
            RagError::EmbeddingError {
                provider: "OpenAI".to_string(),
                message: format!("failed to parse response: {e}"),
            }
        })?;

        Ok(embedding_response.data.into_iter().map(|d| d.embedding).collect())
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}
