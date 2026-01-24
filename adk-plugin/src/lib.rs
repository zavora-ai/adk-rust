//! # adk-plugin
//!
//! Plugin system for ADK-Rust agents.
//!
//! This crate provides a plugin architecture similar to adk-go's plugin package,
//! allowing you to extend agent behavior through callbacks at various lifecycle points.
//!
//! ## Overview
//!
//! Plugins can hook into:
//! - **Run lifecycle**: Before/after the entire agent run
//! - **User messages**: Modify or inspect user input
//! - **Events**: Modify or inspect agent events
//! - **Agent callbacks**: Before/after agent execution
//! - **Model callbacks**: Before/after LLM calls
//! - **Tool callbacks**: Before/after tool execution
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use adk_plugin::{Plugin, PluginConfig, PluginManager};
//!
//! // Create a logging plugin
//! let logging_plugin = Plugin::new(PluginConfig {
//!     name: "logging".to_string(),
//!     on_user_message: Some(Box::new(|ctx, content| {
//!         Box::pin(async move {
//!             println!("User said: {:?}", content);
//!             Ok(None) // Don't modify
//!         })
//!     })),
//!     on_event: Some(Box::new(|ctx, event| {
//!         Box::pin(async move {
//!             println!("Event: {:?}", event);
//!             Ok(None) // Don't modify
//!         })
//!     })),
//!     ..Default::default()
//! });
//!
//! // Create plugin manager
//! let manager = PluginManager::new(vec![logging_plugin]);
//!
//! // Use with Runner
//! let runner = Runner::new(RunnerConfig {
//!     plugin_manager: Some(manager),
//!     ..config
//! });
//! ```

mod callbacks;
mod manager;
mod plugin;

pub use callbacks::*;
pub use manager::{PluginManager, PluginManagerConfig};
pub use plugin::{Plugin, PluginBuilder, PluginConfig};
