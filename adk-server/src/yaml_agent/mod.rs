//! YAML Agent Definition loading and hot reload.
//!
//! This module provides:
//! - [`schema`] — YAML schema types for agent definitions
//! - [`loader`] — Agent config loader for parsing and resolving YAML files
//! - [`watcher`] — Hot reload watcher for filesystem changes
//!
//! Enabled by the `yaml-agent` feature flag.

pub mod loader;
pub mod schema;
pub mod watcher;

// Re-export key types for convenient access.
pub use loader::{AgentConfigLoader, ModelFactory};
pub use schema::{
    McpToolReference, ModelConfig, SubAgentReference, ToolReference, YamlAgentDefinition,
};
pub use watcher::HotReloadWatcher;
