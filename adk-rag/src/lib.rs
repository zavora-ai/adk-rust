//! # adk-rag
#![allow(clippy::result_large_err)]
//!
//! Retrieval-Augmented Generation for ADK-Rust agents.
//!
//! This crate provides a modular, trait-based RAG system with pluggable
//! embedding providers, vector stores, chunking strategies, and rerankers.
//! A [`RagPipeline`](pipeline) orchestrates the full ingest-and-query workflow,
//! and a [`RagTool`](tool) exposes retrieval as an `adk_core::Tool` for
//! agentic use.
//!
//! ## Features
//!
//! All external backends are feature-gated. The default feature set includes
//! only core traits, the in-memory vector store, and chunking implementations.
//!
//! | Feature      | What it enables                          |
//! |--------------|------------------------------------------|
//! | `gemini`     | `GeminiEmbeddingProvider` via adk-gemini  |
//! | `openai`     | `OpenAIEmbeddingProvider` via reqwest     |
//! | `qdrant`     | `QdrantVectorStore` via qdrant-client     |
//! | `lancedb`    | `LanceDBVectorStore` via lancedb          |
//! | `pgvector`   | `PgVectorStore` via sqlx                  |
//! | `agent-retrieval` | `AgentRetrievalStore` via Agent Retrieval (Vector Search 2.0) |
//! | `surrealdb`  | `SurrealVectorStore` via surrealdb        |
//! | `vertex-rag` | Vertex AI RAG Engine client + retrieval tool |
//! | `full`       | `gemini`, `openai`, `qdrant`, `lancedb`, `pgvector`, `surrealdb` |
//!
//! ## Example
//!
//! The pipeline below runs offline with a toy embedder; swap in
//! `GeminiEmbeddingProvider` or `OpenAIEmbeddingProvider` for real embeddings.
//!
//! ```rust
//! use std::sync::Arc;
//!
//! use adk_rag::{
//!     Document, EmbeddingProvider, InMemoryVectorStore, RagConfig, RagPipeline, RecursiveChunker,
//!     Scope,
//! };
//! use async_trait::async_trait;
//!
//! struct LengthEmbedder;
//!
//! #[async_trait]
//! impl EmbeddingProvider for LengthEmbedder {
//!     async fn embed(&self, text: &str) -> adk_rag::Result<Vec<f32>> {
//!         Ok(vec![text.len() as f32, 1.0])
//!     }
//!
//!     fn dimensions(&self) -> usize {
//!         2
//!     }
//! }
//!
//! # #[tokio::main]
//! # async fn main() -> adk_rag::Result<()> {
//! let pipeline = RagPipeline::builder()
//!     .config(RagConfig::default())
//!     .embedding_provider(Arc::new(LengthEmbedder))
//!     .vector_store(Arc::new(InMemoryVectorStore::new()))
//!     .chunker(Arc::new(RecursiveChunker::new(512, 100)))
//!     .build()?;
//!
//! pipeline.create_collection("docs").await?;
//! let document = Document::new("intro", "Rust is a systems programming language.")
//!     .with_source_uri("https://example.com/rust")
//!     .with_title("About Rust");
//!
//! // Every read and write names a scope; an application with one tenant uses this one.
//! let rag = pipeline.scoped(Scope::single_tenant());
//! rag.ingest("docs", &document).await?;
//!
//! let results = rag.query("docs", "systems language").await?;
//! assert_eq!(results[0].chunk.document_id, "intro");
//! assert_eq!(results[0].chunk.source_uri.as_deref(), Some("https://example.com/rust"));
//! # Ok(())
//! # }
//! ```

pub mod chunking;
pub mod config;
pub mod document;
pub mod embedding;
pub mod error;
pub mod filter;
pub mod inmemory;
pub mod pipeline;
pub mod reranker;
pub mod scope;
pub mod subject;
pub mod tool;
pub mod vectorstore;

#[cfg(feature = "agent-retrieval")]
pub mod agent_retrieval;
#[cfg(feature = "gemini")]
pub mod gemini;
#[cfg(feature = "lancedb")]
pub mod lancedb;
#[cfg(feature = "onnx")]
pub mod onnx;
#[cfg(feature = "openai")]
pub mod openai;
#[cfg(feature = "pgvector")]
pub mod pgvector;
#[cfg(feature = "qdrant")]
pub mod qdrant;
#[cfg(feature = "surrealdb")]
pub mod surrealdb;
#[cfg(feature = "vertex-rag")]
pub mod vertex_rag;

#[cfg(feature = "agent-retrieval")]
pub use agent_retrieval::{AgentRetrievalConfig, AgentRetrievalStore};
pub use chunking::{Chunker, FixedSizeChunker, MarkdownChunker, RecursiveChunker};
pub use config::{RagConfig, RagConfigBuilder};
pub use document::{Chunk, Document, SearchResult};
pub use embedding::EmbeddingProvider;
pub use error::{RagError, Result};
pub use filter::{Field, Filter};
pub use inmemory::InMemoryVectorStore;
pub use pipeline::{QueryOptions, RagPipeline, RagPipelineBuilder, ScopedRag};
pub use reranker::{NoOpReranker, Reranker};
pub use scope::Scope;
pub use subject::{RagSubjectData, SUBJECT_METADATA_KEY, SubjectPartition};
pub use tool::RagTool;
pub use vectorstore::{CollectionSpec, VectorQuery, VectorStore};

#[cfg(feature = "gemini")]
pub use gemini::GeminiEmbeddingProvider;
#[cfg(feature = "lancedb")]
pub use lancedb::LanceDBVectorStore;
#[cfg(feature = "onnx")]
pub use onnx::{OnnxEmbeddingConfig, OnnxEmbeddingProvider, OnnxPooling};
#[cfg(feature = "openai")]
pub use openai::OpenAIEmbeddingProvider;
#[cfg(feature = "pgvector")]
pub use pgvector::PgVectorStore;
#[cfg(feature = "qdrant")]
pub use qdrant::QdrantVectorStore;
#[cfg(feature = "surrealdb")]
pub use surrealdb::SurrealVectorStore;
#[cfg(feature = "vertex-rag")]
pub use vertex_rag::{
    RetrieveContextsRequest, VertexAiRagRetrievalTool, VertexRagConfig, VertexRagEngineClient,
};
