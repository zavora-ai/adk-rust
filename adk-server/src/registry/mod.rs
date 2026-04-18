//! Agent Registry REST API.
//!
//! This module provides:
//! - [`types::AgentCard`] — Agent metadata following the A2A protocol schema
//! - [`store::AgentRegistryStore`] — Async trait for storage backends
//! - [`store::InMemoryAgentRegistryStore`] — Default in-memory implementation
//! - [`store::AgentFilter`] — Filter criteria for listing agents
//! - [`routes`] — Axum route handlers for CRUD operations
//!
//! Enabled by the `agent-registry` feature flag.

pub mod routes;
pub mod store;
pub mod types;

pub use routes::registry_router;
pub use store::{AgentFilter, AgentRegistryStore, InMemoryAgentRegistryStore};
pub use types::AgentCard;
