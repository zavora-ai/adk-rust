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
//! ```rust,no_run
//! use adk_memory::InMemoryMemoryService;
//!
//! let service = InMemoryMemoryService::new();
//!
//! // Memory is automatically searched and injected
//! // when configured via LlmAgentBuilder::include_memory()
//! ```
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
//! - Semantic search queries
//! - Six backends: InMemory, SQLite, PostgreSQL, Redis, MongoDB, Neo4j
//! - Versioned schema migrations
//! - GDPR `delete_user` across all projects

pub mod adapter;
pub mod inmemory;
pub mod migration;
pub mod service;
pub mod text;

#[cfg(any(feature = "database-memory", feature = "mongodb-memory", feature = "neo4j-memory"))]
pub mod embedding;
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

pub use adapter::MemoryServiceAdapter;
pub use inmemory::InMemoryMemoryService;
pub use service::{MemoryEntry, MemoryService, SearchRequest, SearchResponse, validate_project_id};

#[cfg(any(feature = "database-memory", feature = "mongodb-memory", feature = "neo4j-memory"))]
pub use embedding::EmbeddingProvider;
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
