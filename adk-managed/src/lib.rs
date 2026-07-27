//! # adk-managed
//!
//! Managed agent runtime for ADK-Rust — a provider-neutral, resumable
//! agent execution engine.
//!
//! ## Overview
//!
//! `adk-managed` provides the `ManagedAgentRuntime` trait and its default implementation.
//! It takes a declarative `ManagedAgentDef`, builds a runnable agent, and operates it as
//! a resumable, event-streaming background session. The runtime composes existing
//! shipping components behind a unified lifecycle trait.
//!
//! ## Architecture
//!
//! The runtime is a **library**, not a service. The platform hosts it. This means:
//!
//! - **Testable in isolation**: Zero HTTP/auth/billing dependencies
//! - **Embeddable**: Self-hosted deployments use the same runtime trait directly
//! - **Swappable platform**: Different platforms can host the same runtime
//! - **Provider-neutral**: Identical event sequences regardless of model provider
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use adk_managed::types::ManagedAgentDef;
//!
//! // Define an agent declaratively
//! let def = ManagedAgentDef {
//!     name: "my-agent".to_string(),
//!     model: ModelRef::Shorthand("gemini-2.5-flash".to_string()),
//!     system_prompt: "You are a helpful assistant.".to_string(),
//!     // ...
//! };
//! ```

pub mod agent_builder;
pub mod checkpoint;
pub mod default_runtime;
pub mod event_mapping;
pub mod parking;
pub mod replay;
pub mod resolver;
pub mod runtime;
pub mod schema_normalization;
pub mod sequence;
pub mod session_loop;
pub mod state_store;
pub mod testing;
pub mod types;
pub mod usage;

pub use agent_builder::{BuildError, ManagedBuiltinTool, ManagedCustomTool, build_agent};
pub use checkpoint::{CheckpointManager, RunState};
pub use default_runtime::DefaultManagedAgentRuntime;
pub use event_mapping::{
    RunnerOutput, ToolKind, custom_tool_use_id, map_runner_output, requires_parking,
};
pub use parking::ToolParkingLot;
pub use replay::{create_event_stream, get_seq};
pub use resolver::{DefaultModelResolver, ModelResolver, ResolverError, ResolverResult};
pub use runtime::{
    AgentHandle, EnvironmentConfig, ManagedAgentRuntime, ManagedOwner, SessionHandle,
};
pub use schema_normalization::{normalize_for_provider, representative_mcp_schema};
pub use sequence::SequenceCounter;
pub use session_loop::SessionLoop;
pub use state_store::{
    Durability, InMemoryManagedStateStore, ManagedSessionState, ManagedStateStore,
};
pub use testing::{ScriptedLlm, ScriptedToolCall, ScriptedTurn};
pub use usage::{SessionUsageTracker, UsageReport};
