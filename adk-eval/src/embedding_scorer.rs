//! Embedding-based semantic similarity scorer.
//!
//! Computes cosine similarity between embedding vectors of expected and actual
//! response texts using an [`EmbeddingProvider`] implementation.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_eval::EmbeddingScorer;
//! use adk_memory::EmbeddingProvider;
//! use std::sync::Arc;
//!
//! let provider: Arc<dyn EmbeddingProvider> = /* your provider */;
//! let scorer = EmbeddingScorer::new(provider);
//! let score = scorer.score("expected text", "actual text").await?;
//! assert!((0.0..=1.0).contains(&score));
//! ```

use std::sync::Arc;

use adk_memory::EmbeddingProvider;

use crate::error::{EvalError, Result};

/// Scores semantic similarity between texts using embedding vectors.
///
/// Wraps an [`EmbeddingProvider`] to generate embeddings and computes
/// cosine similarity between expected and actual response texts.
pub struct EmbeddingScorer {
    provider: Arc<dyn EmbeddingProvider>,
}

impl EmbeddingScorer {
    /// Create a new `EmbeddingScorer` with the given embedding provider.
    pub fn new(provider: Arc<dyn EmbeddingProvider>) -> Self {
        Self { provider }
    }

    /// Compute cosine similarity between expected and actual response texts.
    ///
    /// Returns a score in \[0.0, 1.0\] where 1.0 indicates identical meaning.
    ///
    /// # Errors
    ///
    /// Returns [`EvalError::EmbeddingError`] if embedding generation fails or
    /// if the provider returns vectors of mismatched dimensions.
    pub async fn score(&self, expected: &str, actual: &str) -> Result<f64> {
        let texts = vec![expected.to_string(), actual.to_string()];

        let embeddings =
            self.provider.embed(&texts).await.map_err(|e| {
                EvalError::EmbeddingError(format!("embedding generation failed: {e}"))
            })?;

        if embeddings.len() < 2 {
            return Err(EvalError::EmbeddingError(
                "provider returned fewer than 2 embeddings".to_string(),
            ));
        }

        let expected_vec = &embeddings[0];
        let actual_vec = &embeddings[1];

        if expected_vec.len() != actual_vec.len() {
            return Err(EvalError::EmbeddingError(format!(
                "dimension mismatch: expected vector has {} dimensions, actual has {}",
                expected_vec.len(),
                actual_vec.len()
            )));
        }

        Ok(cosine_similarity(expected_vec, actual_vec))
    }
}

/// Compute cosine similarity between two vectors.
///
/// Returns a value clamped to \[0.0, 1.0\] for use as a scoring metric.
/// Returns 0.0 if either vector is a zero vector (has zero magnitude).
///
/// # Panics
///
/// Does not panic. Returns 0.0 for edge cases (empty vectors, zero vectors,
/// mismatched dimensions).
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let mut dot_product = 0.0_f64;
    let mut magnitude_a = 0.0_f64;
    let mut magnitude_b = 0.0_f64;

    for (ai, bi) in a.iter().zip(b.iter()) {
        let ai_f64 = f64::from(*ai);
        let bi_f64 = f64::from(*bi);
        dot_product += ai_f64 * bi_f64;
        magnitude_a += ai_f64 * ai_f64;
        magnitude_b += bi_f64 * bi_f64;
    }

    let magnitude_a = magnitude_a.sqrt();
    let magnitude_b = magnitude_b.sqrt();

    // Zero vector — no meaningful direction
    if magnitude_a == 0.0 || magnitude_b == 0.0 {
        return 0.0;
    }

    let similarity = dot_product / (magnitude_a * magnitude_b);

    // Clamp to [0.0, 1.0] — cosine similarity can be negative for opposing
    // vectors but we use the clamped value as a [0, 1] score.
    similarity.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity_identical_vectors() {
        let v = vec![1.0_f32, 2.0, 3.0];
        let score = cosine_similarity(&v, &v);
        assert!((score - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_cosine_similarity_orthogonal_vectors() {
        let a = vec![1.0_f32, 0.0];
        let b = vec![0.0_f32, 1.0];
        let score = cosine_similarity(&a, &b);
        assert!(score.abs() < 1e-10);
    }

    #[test]
    fn test_cosine_similarity_zero_vector() {
        let a = vec![1.0_f32, 2.0, 3.0];
        let zero = vec![0.0_f32, 0.0, 0.0];
        assert_eq!(cosine_similarity(&a, &zero), 0.0);
        assert_eq!(cosine_similarity(&zero, &a), 0.0);
        assert_eq!(cosine_similarity(&zero, &zero), 0.0);
    }

    #[test]
    fn test_cosine_similarity_mismatched_dimensions() {
        let a = vec![1.0_f32, 2.0];
        let b = vec![1.0_f32, 2.0, 3.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn test_cosine_similarity_empty_vectors() {
        let empty: Vec<f32> = vec![];
        assert_eq!(cosine_similarity(&empty, &empty), 0.0);
    }

    #[test]
    fn test_cosine_similarity_opposite_vectors_clamped() {
        let a = vec![1.0_f32, 0.0];
        let b = vec![-1.0_f32, 0.0];
        // Opposite vectors have cosine similarity of -1.0, clamped to 0.0
        let score = cosine_similarity(&a, &b);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_cosine_similarity_result_in_range() {
        let a = vec![1.0_f32, 2.0, 3.0, 4.0];
        let b = vec![4.0_f32, 3.0, 2.0, 1.0];
        let score = cosine_similarity(&a, &b);
        assert!((0.0..=1.0).contains(&score));
    }
}
