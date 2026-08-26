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
//! | `full`       | All of the above except `vertex-rag` (external infrastructure) |

pub mod chunking;
pub mod config;
pub mod document;
pub mod embedding;
pub mod error;
pub mod inmemory;
pub mod pipeline;
pub mod reranker;
pub mod tool;
pub mod vectorstore;

#[cfg(feature = "agent-retrieval")]
pub mod agent_retrieval;
#[cfg(feature = "gemini")]
pub mod gemini;
#[cfg(feature = "lancedb")]
pub mod lancedb;
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
pub use inmemory::InMemoryVectorStore;
pub use pipeline::{RagPipeline, RagPipelineBuilder};
pub use reranker::{NoOpReranker, Reranker};
pub use tool::RagTool;
pub use vectorstore::VectorStore;

#[cfg(feature = "gemini")]
pub use gemini::GeminiEmbeddingProvider;
#[cfg(feature = "lancedb")]
pub use lancedb::LanceDBVectorStore;
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
