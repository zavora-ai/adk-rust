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

pub mod inmemory;
pub mod service;

pub use inmemory::InMemoryMemoryService;
pub use service::{MemoryEntry, MemoryService, SearchRequest, SearchResponse};
