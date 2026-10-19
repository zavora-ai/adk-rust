//! # adk-memory
//!
//! Semantic memory and search for ADK agents.
//!
//! ## Overview
//!
//! This crate provides long-term memory capabilities:
//!
//! - [`InMemoryMemoryService`] - Simple in-memory memory storage
//! - [`MemoryService`] - Trait for custom backends
//! - [`MemoryEntry`] - Structured memory with metadata
//! - [`MemoryServiceAdapter`] - Bridge to [`adk_core::Memory`] with optional project scope
//! - [`validate_project_id`] - Project identifier validation
//!
//! ## Quick Start
//!
//! ```rust
//! use adk_core::Content;
//! use adk_memory::{InMemoryMemoryService, MemoryEntry, MemoryService, SearchRequest};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> adk_core::Result<()> {
//! let service = InMemoryMemoryService::new();
//! let entry = MemoryEntry {
//!     content: Content::new("user").with_text("Hometown: Nairobi."),
//!     author: "user".to_string(),
//!     timestamp: chrono::Utc::now(),
//! };
//! service.add_session("app", "user", "session", vec![entry]).await?;
//!
//! let found = service
//!     .search(SearchRequest {
//!         query: "hometown".into(),
//!         user_id: "user".into(),
//!         app_name: "app".into(),
//!         limit: None,
//!         min_score: None,
//!         project_id: None,
//!     })
//!     .await?;
//! assert_eq!(found.memories.len(), 1);
//! # Ok(())
//! # }
//! ```
//!
//! The runner attaches a memory service to each invocation (`ToolContext::search_memory`);
//! `adk-tool`'s `LoadMemoryTool` and `PreloadMemoryTool` (feature `memory-tools`) expose it
//! to the model.
//!
//! ## Project-Scoped Memory
//!
//! Memories can be isolated by project within a user. The isolation key is
//! `(app_name, user_id, project_id?)`:
//!
//! - **Global entries** (`project_id = None`): visible in all contexts.
//! - **Project entries** (`project_id = Some(id)`): visible only within that project.
//! - **Project search**: returns global + matching project entries.
//! - **Global search**: returns only global entries.
//!
//! ```rust,ignore
//! use adk_memory::{InMemoryMemoryService, MemoryService, MemoryServiceAdapter};
//! use std::sync::Arc;
//!
//! let service = Arc::new(InMemoryMemoryService::new());
//!
//! // Store entries in a project
//! service.add_session_to_project("app", "user", "sess", "my-project", entries).await?;
//!
//! // Adapter scoped to a project
//! let adapter = MemoryServiceAdapter::new(service, "app", "user")
//!     .with_project_id("my-project");
//! ```
//!
//! ## Features
//!
//! - Per-user and per-project memory isolation
//! - Keyword search, and vector search with an `EmbeddingProvider` on PostgreSQL, MongoDB,
//!   and Neo4j
//! - Backends: InMemory, SQLite, PostgreSQL, Redis, MongoDB, Neo4j, Vertex AI Memory Bank,
//!   and the SQLite knowledge graph `GraphMemoryService`
//! - Versioned schema migrations
//! - GDPR `delete_user` across all projects

pub mod adapter;
pub mod inmemory;
pub mod migration;
pub mod service;
pub mod text;

#[cfg(any(
    feature = "database-memory",
    feature = "mongodb-memory",
    feature = "neo4j-memory",
    feature = "embedding-trait"
))]
pub mod embedding;
#[cfg(feature = "graph-memory")]
pub mod graph;
#[cfg(any(feature = "redis-memory", feature = "neo4j-memory"))]
mod key;
#[cfg(feature = "mongodb-memory")]
pub mod mongodb;
#[cfg(feature = "neo4j-memory")]
pub mod neo4j;
#[cfg(feature = "database-memory")]
pub mod postgres;
#[cfg(feature = "redis-memory")]
pub mod redis;
#[cfg(feature = "sqlite-memory")]
pub mod sqlite;
#[cfg(feature = "vertex-memory")]
pub mod vertex;

pub use adapter::MemoryServiceAdapter;
pub use inmemory::InMemoryMemoryService;
pub use service::{MemoryEntry, MemoryService, SearchRequest, SearchResponse, validate_project_id};

#[cfg(any(
    feature = "database-memory",
    feature = "mongodb-memory",
    feature = "neo4j-memory",
    feature = "embedding-trait"
))]
pub use embedding::EmbeddingProvider;
#[cfg(feature = "graph-memory")]
pub use graph::{
    CreateEntityInput, CreateRelationInput, Entity, GraphMemoryService, GraphSearchResult,
    Observation, Relation,
};
#[cfg(feature = "mongodb-memory")]
pub use mongodb::MongoMemoryService;
#[cfg(feature = "neo4j-memory")]
pub use neo4j::Neo4jMemoryService;
#[cfg(feature = "database-memory")]
pub use postgres::{PostgresMemoryService, PostgresMemoryServiceBuilder, VectorIndexType};
#[cfg(feature = "redis-memory")]
pub use redis::{RedisMemoryConfig, RedisMemoryService};
#[cfg(feature = "sqlite-memory")]
pub use sqlite::SqliteMemoryService;
#[cfg(feature = "vertex-memory")]
pub use vertex::{VertexAiMemoryBankService, VertexAiMemoryConfig};
