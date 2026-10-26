//! Embedding provider trait for vector-based memory search.
//!
//! Provides [`EmbeddingProvider`], a trait for pluggable embedding models
//! used by `PostgresMemoryService`.

use adk_core::Result;
use async_trait::async_trait;

/// Generates vector embeddings from text content.
///
/// # Example
///
/// ```rust
/// use adk_memory::EmbeddingProvider;
///
/// struct Lengths;
///
/// #[async_trait::async_trait]
/// impl EmbeddingProvider for Lengths {
///     async fn embed(&self, texts: &[String]) -> adk_core::Result<Vec<Vec<f32>>> {
///         Ok(texts.iter().map(|text| vec![text.len() as f32, 1.0]).collect())
///     }
///
///     fn dimensions(&self) -> usize {
///         2
///     }
///
///     fn model_id(&self) -> Option<&str> {
///         Some("example/lengths")
///     }
/// }
///
/// assert_eq!(Lengths.model_id(), Some("example/lengths"));
/// ```
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Generate embeddings for a batch of texts.
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    /// Return the dimensionality of the embedding vectors.
    fn dimensions(&self) -> usize;

    /// A stable id of the model behind the vectors, such as
    /// `openai/text-embedding-3-small`, so a caller that stores vectors never compares
    /// one model's vectors with another's that happen to share a length.
    ///
    /// The default, `None`, leaves callers to tell models apart by
    /// [`dimensions`](Self::dimensions) alone.
    fn model_id(&self) -> Option<&str> {
        None
    }
}
