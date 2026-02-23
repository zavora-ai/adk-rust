//! # adk-runner
//!
//! Agent execution runtime for ADK.
//!
//! ## Overview
//!
//! This crate provides the execution runtime:
//!
//! - [`Runner`] - Manages agent execution with full context
//! - [`RunnerConfig`] - Configuration for the runner
//! - [`InvocationContext`] - Execution context implementation
//! - [`Callbacks`] - Hook points during execution
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use adk_runner::{Runner, RunnerConfig};
//! use std::sync::Arc;
//!
//! // Configure runner with services
//! // let config = RunnerConfig {
//! //     app_name: "my_app".to_string(),
//! //     session_service: sessions,
//! //     artifact_service: Some(artifacts),
//! //     memory_service: None,
//! // };
//! //
//! // let runner = Runner::new(config);
//! ```
//!
//! ## Features
//!
//! - Automatic session management
//! - Memory injection
//! - Artifact handling
//! - Callback hooks at every stage

mod cache;
mod callbacks;
mod context;
mod runner;

pub use callbacks::{
    AfterModelCallback, AfterToolCallback, BeforeModelCallback, BeforeToolCallback, Callbacks,
};
pub use context::{InvocationContext, MutableSession};
pub use runner::{Runner, RunnerConfig};

// Re-export compaction types for convenience
pub use adk_core::{BaseEventsSummarizer, EventsCompactionConfig};

// Re-export cache types for convenience
pub use adk_core::{CacheCapable, ContextCacheConfig};
pub use cache::{CacheMetrics, CachePerformanceAnalyzer};
