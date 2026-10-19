//! # adk-retry-reflect
//!
//! A Retry & Reflect plugin for ADK-Rust that intercepts tool call failures and
//! injects structured reflection prompts to help the agent self-correct.
//!
//! Instead of immediately propagating errors, the plugin constructs a reflection
//! prompt containing the error details, original arguments, and guidance text,
//! then returns it as a modified tool result so the agent can retry with corrected
//! arguments on the next turn.
//!
//! ## Features
//!
//! - **Per-tool retry limits** with configurable defaults and per-tool overrides
//! - **Global retry limit** to prevent runaway retry loops
//! - **Configurable backoff** (none, fixed, or exponential with ceiling)
//! - **Tool eligibility filtering** via allowlist or denylist
//! - **Customizable reflection templates** with placeholder substitution
//! - **Global failure tracking** for circuit-breaker patterns
//! - **Structured tracing events** for monitoring retry behavior
//!
//! ## Quick Start
//!
//! ```rust
//! use std::time::Duration;
//! use adk_retry_reflect::RetryReflectPluginBuilder;
//!
//! // Create a plugin with exponential backoff
//! let plugin = RetryReflectPluginBuilder::new()
//!     .max_retries(3)
//!     .backoff_exponential(Duration::from_millis(100))
//!     .max_backoff(Duration::from_secs(10))
//!     .build()
//!     .expect("valid configuration");
//!
//! // Register it on an agent with `LlmAgentBuilder::enhanced_plugin(Arc::new(plugin))`
//! // (adk-agent's `enhanced-plugins` feature), or run it in an `EnhancedPluginManager`.
//! let manager = adk_plugin::EnhancedPluginManager::new(vec![std::sync::Arc::new(plugin)]);
//! assert_eq!(manager.plugin_count(), 1);
//! ```
//!
//! ## Configuration
//!
//! Use the [`RetryReflectPluginBuilder`] for fluent configuration:
//!
//! ```rust
//! use std::time::Duration;
//! use adk_retry_reflect::RetryReflectPluginBuilder;
//!
//! let plugin = RetryReflectPluginBuilder::new()
//!     .max_retries(5)
//!     .per_tool_limit("flaky_api", 10)
//!     .global_limit(20)
//!     .backoff_exponential(Duration::from_millis(200))
//!     .max_backoff(Duration::from_secs(30))
//!     .allowlist(["flaky_api", "search_tool"])
//!     .priority(200)
//!     .enable_global_tracking(15)
//!     .build()
//!     .expect("valid configuration");
//! ```

pub mod backoff;
pub mod builder;
pub mod config;
pub mod detection;
pub mod error;
pub mod filter;
pub mod plugin;
pub mod template;
pub mod tracker;

pub use builder::RetryReflectPluginBuilder;
pub use config::{BackoffStrategy, RetryReflectConfig, ToolFilter};
pub use error::RetryReflectError;
pub use plugin::RetryReflectPlugin;
pub use tracker::{GlobalRetryTracker, RetryTracker};
