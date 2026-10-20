//! `RagEmbedder`: adk-rag's embedders as adk-memory's batch trait (spec 3.1 §7.15.2,
//! feature `memory-embeddings`).

use std::sync::Arc;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use async_trait::async_trait;

/// An `adk_rag::EmbeddingProvider` as an [`adk_memory::EmbeddingProvider`], for
/// [`HarnessAgentBuilder::memory_embedder`](crate::HarnessAgentBuilder::memory_embedder)
/// (spec 3.1 §7.15.2).
///
/// adk-rag embeds one text at a time or a batch of `&str`; adk-memory embeds a batch of
/// `String`. [`openai`](Self::openai) and [`gemini`](Self::gemini) build the two
/// providers adk-rag ships.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_harness::{HarnessAgent, LongTermMemoryConfig, RagEmbedder};
///
/// # fn build(model: Arc<dyn adk_core::Llm>) -> adk_core::Result<HarnessAgent> {
/// let embedder = RagEmbedder::gemini(&std::env::var("GOOGLE_API_KEY").unwrap_or_default())?;
/// HarnessAgent::builder("harness")
///     .model(model)
///     .workspace(adk_devtools::Workspace::new("."))
///     .long_term_memory(LongTermMemoryConfig::default())
///     .memory_embedder(Arc::new(embedder))
///     .build()
/// # }
/// ```
pub struct RagEmbedder {
    inner: Arc<dyn adk_rag::EmbeddingProvider>,
}

impl std::fmt::Debug for RagEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RagEmbedder").field("dimensions", &self.inner.dimensions()).finish()
    }
}

fn embedding_error(error: adk_rag::RagError) -> AdkError {
    AdkError::new(
        ErrorComponent::Memory,
        ErrorCategory::Unavailable,
        "harness.memory.embedding_failed",
        format!("the fact embedder failed: {error}"),
    )
}

impl RagEmbedder {
    /// Wraps `inner`.
    pub fn new(inner: Arc<dyn adk_rag::EmbeddingProvider>) -> Self {
        Self { inner }
    }

    /// OpenAI's `text-embedding-3-small` with `api_key`.
    ///
    /// # Errors
    ///
    /// Returns an error when the client cannot be built.
    pub fn openai(api_key: &str) -> Result<Self> {
        Ok(Self::new(Arc::new(
            adk_rag::openai::OpenAIEmbeddingProvider::new(api_key).map_err(embedding_error)?,
        )))
    }

    /// Gemini's `gemini-embedding-2` with `api_key`.
    ///
    /// # Errors
    ///
    /// Returns an error when the client cannot be built.
    pub fn gemini(api_key: &str) -> Result<Self> {
        Ok(Self::new(Arc::new(
            adk_rag::GeminiEmbeddingProvider::new(api_key).map_err(embedding_error)?,
        )))
    }
}

#[async_trait]
impl adk_memory::EmbeddingProvider for RagEmbedder {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
        let vectors = self.inner.embed_batch(&texts).await.map_err(embedding_error)?;
        if vectors.len() != texts.len() {
            return Err(AdkError::new(
                ErrorComponent::Memory,
                ErrorCategory::Internal,
                "harness.memory.embedding_failed",
                format!(
                    "the fact embedder returned {} vectors for {} texts",
                    vectors.len(),
                    texts.len()
                ),
            ));
        }
        Ok(vectors)
    }

    fn dimensions(&self) -> usize {
        self.inner.dimensions()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_memory::EmbeddingProvider as _;

    struct Echo;

    #[async_trait]
    impl adk_rag::EmbeddingProvider for Echo {
        async fn embed(&self, text: &str) -> adk_rag::Result<Vec<f32>> {
            Ok(vec![text.len() as f32, 1.0])
        }

        fn dimensions(&self) -> usize {
            2
        }
    }

    #[tokio::test]
    async fn batches_are_embedded_one_vector_per_text() {
        let embedder = RagEmbedder::new(Arc::new(Echo));
        let vectors = embedder.embed(&["a".into(), "abc".into()]).await.unwrap();
        assert_eq!(vectors, vec![vec![1.0, 1.0], vec![3.0, 1.0]]);
        assert_eq!(embedder.dimensions(), 2);
    }
}
