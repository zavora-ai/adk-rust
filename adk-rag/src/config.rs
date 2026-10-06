//! Configuration for the RAG pipeline.

use serde::{Deserialize, Serialize};

use crate::error::{RagError, Result};

/// Configuration parameters for the RAG pipeline.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RagConfig {
    /// Maximum chunk size in bytes of UTF-8.
    pub chunk_size: usize,
    /// Number of bytes shared by consecutive chunks.
    pub chunk_overlap: usize,
    /// Number of top results to return from vector search.
    pub top_k: usize,
    /// Minimum similarity score for results (results below this are filtered out).
    pub similarity_threshold: f32,
}

impl Default for RagConfig {
    fn default() -> Self {
        Self { chunk_size: 512, chunk_overlap: 100, top_k: 10, similarity_threshold: 0.0 }
    }
}

impl RagConfig {
    /// Create a new builder for constructing a [`RagConfig`].
    pub fn builder() -> RagConfigBuilder {
        RagConfigBuilder::default()
    }
}

/// Builder for constructing a validated [`RagConfig`].
#[derive(Debug, Clone, Default)]
pub struct RagConfigBuilder {
    config: RagConfig,
}

impl RagConfigBuilder {
    /// Set the maximum chunk size in bytes of UTF-8.
    pub fn chunk_size(mut self, size: usize) -> Self {
        self.config.chunk_size = size;
        self
    }

    /// Set the overlap between consecutive chunks in bytes.
    pub fn chunk_overlap(mut self, overlap: usize) -> Self {
        self.config.chunk_overlap = overlap;
        self
    }

    /// Set the number of top results to return from vector search.
    pub fn top_k(mut self, k: usize) -> Self {
        self.config.top_k = k;
        self
    }

    /// Set the minimum similarity threshold for filtering results.
    pub fn similarity_threshold(mut self, threshold: f32) -> Self {
        self.config.similarity_threshold = threshold;
        self
    }

    /// Build the [`RagConfig`], validating that parameters are consistent.
    ///
    /// # Errors
    ///
    /// Returns [`RagError::ConfigError`] if:
    /// - `chunk_overlap >= chunk_size`
    /// - `top_k == 0`
    pub fn build(self) -> Result<RagConfig> {
        if self.config.chunk_overlap >= self.config.chunk_size {
            return Err(RagError::ConfigError(format!(
                "chunk_overlap ({}) must be less than chunk_size ({})",
                self.config.chunk_overlap, self.config.chunk_size
            )));
        }
        if self.config.top_k == 0 {
            return Err(RagError::ConfigError("top_k must be greater than zero".to_string()));
        }
        Ok(self.config)
    }
}
