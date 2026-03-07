//! Embedding provider trait for vector-based memory search.
//!
//! Provides [`EmbeddingProvider`], a trait for pluggable embedding models
//! used by [`PostgresMemoryService`](super::postgres::PostgresMemoryService).

use adk_core::Result;
use async_trait::async_trait;

/// Generates vector embeddings from text content.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Generate embeddings for a batch of texts.
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    /// Return the dimensionality of the embedding vectors.
    fn dimensions(&self) -> usize;
}
