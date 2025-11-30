//! # Embedding Module
//!
//! This module provides functionality for generating text embeddings using the Gemini API.
//! It includes support for both single and batch embedding operations with various task types
//! for optimization.

pub mod builder;
pub mod model;

pub use builder::EmbedBuilder;
pub use model::{
    BatchContentEmbeddingResponse, BatchEmbedContentsRequest, ContentEmbedding,
    ContentEmbeddingResponse, EmbedContentRequest, TaskType,
};
