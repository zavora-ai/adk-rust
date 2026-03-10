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
//! ## Features
//!
//! - Per-user memory isolation
//! - Semantic search queries
//! - Metadata filtering
//! - Automatic context injection

pub mod adapter;
pub mod inmemory;
pub mod migration;
pub mod service;

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
pub use service::{MemoryEntry, MemoryService, SearchRequest, SearchResponse};

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
