//! Memory tools for ADK agents.
//!
//! This module provides tools that enable agents to autonomously search their
//! own long-term memory during reasoning:
//!
//! - [`LoadMemoryTool`] — an agent-callable tool for on-demand memory search
//! - [`PreloadMemoryTool`] — automatically loads relevant memories at turn start
//! - [`MemoryToolConfig`] — shared configuration for both tools
//!
//! # Feature Gate
//!
//! This module is only available when the `memory-tools` feature is enabled:
//!
//! ```toml
//! [dependencies]
//! adk-tool = { version = "3.0.0", features = ["memory-tools"] }
//! ```
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_tool::memory::{LoadMemoryTool, PreloadMemoryTool};
//! use adk_memory::InMemoryMemoryService;
//! use std::sync::Arc;
//!
//! let memory_service = Arc::new(InMemoryMemoryService::new());
//!
//! // LoadMemoryTool — agent calls during reasoning
//! let load_tool = LoadMemoryTool::builder()
//!     .memory_service(memory_service.clone())
//!     .max_results(5)
//!     .min_relevance_score(0.3)
//!     .build()?;
//!
//! // PreloadMemoryTool — auto-injects at turn start
//! let preload_tool = PreloadMemoryTool::builder()
//!     .memory_service(memory_service.clone())
//!     .max_results(3)
//!     .build()?;
//!
//! // Convert to a before-model callback for automatic injection
//! let callback = preload_tool.into_before_model_callback();
//! ```

pub mod config;
pub mod format;
#[cfg(feature = "graph-memory-tools")]
pub mod graph;
pub mod load_memory;
pub mod preload_memory;

pub use config::{MemoryToolConfig, MemoryToolConfigBuilder};
#[cfg(feature = "graph-memory-tools")]
pub use graph::{GraphMemoryToolset, RelateTool, RememberTool};
pub use load_memory::{LoadMemoryTool, LoadMemoryToolBuilder};
pub use preload_memory::{PreloadMemoryTool, PreloadMemoryToolBuilder};
