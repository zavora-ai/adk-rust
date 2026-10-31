//! # adk-runner
//!
//! Agent execution runtime for ADK.
//!
//! ## Overview
//!
//! This crate provides the execution runtime:
//!
//! - [`Runner`] - Runs an agent tree against a session and streams its events
//! - [`RunnerConfigBuilder`] - Typestate builder returned by [`Runner::builder`]
//! - [`RunnerConfig`] - Struct-literal configuration for [`Runner::new`]
//! - [`InvocationContext`] - Execution context implementation
//! - [`Launcher`] - Minimal stdin/stdout console loop
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use std::sync::Arc;
//!
//! use adk_core::{Agent, Content};
//! use adk_runner::Runner;
//! use adk_session::{CreateRequest, InMemorySessionService, SessionService};
//! use futures::StreamExt;
//!
//! async fn run(agent: Arc<dyn Agent>) -> adk_core::Result<()> {
//!     let sessions = Arc::new(InMemorySessionService::new());
//!     sessions
//!         .create(CreateRequest {
//!             app_name: "my_app".into(),
//!             user_id: "user".into(),
//!             session_id: Some("session".into()),
//!             state: Default::default(),
//!         })
//!         .await?;
//!
//!     let runner = Runner::builder()
//!         .app_name("my_app")
//!         .agent(agent)
//!         .session_service(sessions)
//!         .build()?;
//!
//!     let mut events =
//!         runner.run_str("user", "session", Content::new("user").with_text("Hello")).await?;
//!     while let Some(event) = events.next().await {
//!         let _event = event?;
//!     }
//!     runner.close().await;
//!     Ok(())
//! }
//! ```
//!
//! ## Features
//!
//! - Session loading, event persistence, and state propagation
//! - Artifact and memory services on the invocation context
//! - Transfer restrictions enforced on every transfer
//! - Run budgets, the spend ledger, tool policy, the kill switch, and audit records
//! - Plugin hooks for every model and tool call (with the `plugins` feature)
//! - Steering a running agent with [`Runner::steer`], screened like the run's own message
#![allow(clippy::result_large_err)]
#![deny(missing_docs)]

mod acting;
mod agent_invoker;
pub mod builder;
mod cache;
#[cfg(feature = "context-compaction")]
pub mod compaction;
mod context;
pub mod intra_compaction;
mod launcher;
mod runner;
#[cfg(feature = "sandbox-runner")]
pub mod sandbox_runner;
pub mod spend;
pub mod tool_concurrency;

pub use builder::RunnerConfigBuilder;
pub use context::{InvocationContext, MutableSession};
pub use launcher::Launcher;
pub use runner::{Runner, RunnerConfig, RunnerInvocation};
pub use spend::LlmSpendEstimate;

// Re-export RequestContext for convenience
pub use adk_core::RequestContext;

// Re-export compaction types for convenience
pub use adk_core::IntraCompactionConfig;
pub use adk_core::{BaseEventsSummarizer, EventsCompactionConfig};
pub use intra_compaction::IntraInvocationCompactor;

// Re-export secret service trait for convenience
pub use adk_core::SecretService;

// Re-export the steering receipt types `Runner::steer` returns
pub use adk_core::{SteerOutcome, SteerReceipt};

// Re-export cache types for convenience
pub use adk_core::{CacheCapable, ContextCacheConfig};
pub use cache::{CacheMetrics, CachePerformanceAnalyzer};
