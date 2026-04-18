//! MCP Server Lifecycle Management.
//!
//! This module provides [`McpServerManager`] for managing the full lifecycle of
//! multiple local MCP server child processes. It spawns processes, connects them
//! via `TokioChildProcess` transport into [`McpToolset`](super::McpToolset) instances,
//! monitors health, auto-restarts on crash with exponential backoff, and aggregates
//! tools from all managed servers behind the [`Toolset`](adk_core::Toolset) trait.
//!
//! # Configuration
//!
//! Server configurations are loaded from Kiro's `mcp.json` format via
//! [`McpServerConfig`] and [`RestartPolicy`].
//!
//! # Status Tracking
//!
//! Each managed server has a [`ServerStatus`] indicating its current lifecycle state.

pub(crate) mod config;
pub(crate) mod entry;
#[allow(clippy::module_inception)]
mod manager;
pub(crate) mod status;
mod toolset_impl;

pub use config::{McpServerConfig, RestartPolicy};
pub use status::ServerStatus;

pub use manager::McpServerManager;
