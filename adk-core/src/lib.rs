//! # adk-core
//!
//! Core traits and types for ADK agents, tools, sessions, and events.
#![allow(clippy::result_large_err)]
#![deny(missing_docs)]
//!
//! ## Overview
//!
//! This crate provides the foundational abstractions for the Agent Development Kit:
//!
//! - [`Agent`] - The fundamental trait for all agents
//! - [`Tool`] / [`Toolset`] - For extending agents with custom capabilities
//! - [`Session`] / [`State`] - For managing conversation context
//! - [`Event`] - For streaming agent responses
//! - [`AdkError`] / [`Result`] - Unified error handling
//! - [`SharedState`] / [`SharedStateError`] - Thread-safe key-value store for parallel agent coordination
//! - [`ToolConfirmationPolicy`] / [`ToolConfirmationRequest`] - Human-in-the-loop tool authorization
//!
//! ## What's New in 0.6.0
//!
//! - **`SharedState`**: Concurrent key-value store with `set_shared`, `get_shared`, and
//!   `wait_for_key` (timeout-based blocking) for cross-agent coordination in `ParallelAgent`.
//! - **`shared_state()` on `CallbackContext`**: Default method returning `None` — tools and
//!   callbacks can access `SharedState` when running inside a `ParallelAgent` with shared state enabled.
//! - **`ToolConfirmationPolicy`**: Built-in HITL mechanism — `Never`, `Always`, or `PerTool`
//!   policies that pause execution and emit `ToolConfirmationRequest` events for user approval.
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use adk_core::{Agent, Tool, Event, Result};
//! use std::sync::Arc;
//!
//! // All agents implement the Agent trait
//! // All tools implement the Tool trait
//! // Events are streamed as the agent executes
//! ```
//!
//! ## Core Traits
//!
//! ### Agent
//!
//! The [`Agent`] trait defines the interface for all agents:
//!
//! ```rust,ignore
//! #[async_trait]
//! pub trait Agent: Send + Sync {
//!     fn name(&self) -> &str;
//!     fn description(&self) -> Option<&str>;
//!     async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream>;
//! }
//! ```
//!
//! ### Tool
//!
//! The [`Tool`] trait defines custom capabilities:
//!
//! ```rust,ignore
//! #[async_trait]
//! pub trait Tool: Send + Sync {
//!     fn name(&self) -> &str;
//!     fn description(&self) -> &str;
//!     async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value>;
//! }
//! ```
//!
//! ## State Management
//!
//! State uses typed prefixes for organization:
//!
//! - `user:` - User preferences (persists across sessions)
//! - `app:` - Application state (application-wide)
//! - `temp:` - Temporary data (cleared each turn)

/// Core agent trait and event stream type.
pub mod agent;
/// Dynamic agent loading by name.
pub mod agent_loader;
/// Callback type aliases for agent, model, and tool lifecycle hooks.
pub mod callbacks;
/// Invocation context traits: state, session, artifacts, memory, and run configuration.
pub mod context;
/// Unified structured error type and result alias.
pub mod error;
/// Event types representing agent interactions in a conversation.
pub mod event;
/// Typed identity primitives for app, user, session, and invocation.
pub mod identity;
/// Template-based instruction injection with session state interpolation.
pub mod instruction_template;
/// Intra-turn context compaction configuration.
pub mod intra_compaction;
/// LLM trait, request/response types, and caching configuration.
pub mod model;
/// HTTP request context extracted by auth middleware.
pub mod request_context;
/// Provider-aware JSON Schema normalization for tool declarations.
pub mod schema_adapter;
/// Thread-safe schema cache for tool parameter schemas.
pub mod schema_cache;
/// JSON Schema utility functions.
pub mod schema_utils;
/// Thread-safe shared state for parallel agent coordination.
pub mod shared_state;
/// Tool trait, toolset, execution strategy, and registry.
pub mod tool;
/// Semaphore-based tool concurrency management.
pub mod tool_concurrency;
/// Content, Part, and multimodal data types.
pub mod types;

pub use agent::{Agent, EventStream, ResolvedContext};
pub use agent_loader::{AgentLoader, MultiAgentLoader, SingleAgentLoader};
pub use callbacks::{
    AfterAgentCallback, AfterModelCallback, AfterToolCallback, AfterToolCallbackFull,
    BaseEventsSummarizer, BeforeAgentCallback, BeforeModelCallback, BeforeModelResult,
    BeforeToolCallback, EventsCompactionConfig, GlobalInstructionProvider, InstructionProvider,
    OnToolErrorCallback,
};
pub use context::{
    Artifacts, BackpressurePolicy, CallbackContext, IncludeContents, InvocationContext,
    MAX_STATE_KEY_LEN, Memory, MemoryEntry, ReadonlyContext, ReadonlyState, RunConfig,
    RunConfigBuilder, RuntimeToolset, SecretService, Session, State, StreamingMode,
    ToolCallbackContext, ToolConcurrencyConfig, ToolConfirmationDecision, ToolConfirmationHandler,
    ToolConfirmationPolicy, ToolConfirmationRequest, ToolOutcome, validate_state_key,
};
pub use error::{AdkError, ErrorCategory, ErrorComponent, ErrorDetails, Result, RetryHint};
pub use event::{
    Event, EventActions, EventCompaction, KEY_PREFIX_APP, KEY_PREFIX_TEMP, KEY_PREFIX_USER,
    TOOL_PROGRESS_CALL_ID_KEY, TOOL_PROGRESS_STREAM_KEY, ToolCallView, ToolResultView,
};
pub use identity::{
    AdkIdentity, AppName, ExecutionIdentity, IdentityError, InvocationId, SessionId, UserId,
};
pub use instruction_template::inject_session_state;
pub use intra_compaction::IntraCompactionConfig;
pub use model::{
    CacheCapable, CitationMetadata, CitationSource, ContextCacheConfig, FinishReason,
    GenerateContentConfig, Llm, LlmRequest, LlmResponse, LlmResponseStream, UsageMetadata,
};
pub use request_context::RequestContext;
pub use schema_adapter::{GenericSchemaAdapter, SchemaAdapter};
pub use schema_cache::SchemaCache;
pub use shared_state::{SharedState, SharedStateError};
pub use tool::{
    RetryBudget, Tool, ToolContext, ToolExecutionStrategy, ToolPredicate, ToolRegistry, Toolset,
    ValidationMode,
};
pub use tool_concurrency::{ConcurrencyPermit, ToolConcurrencyManager};
pub use types::{
    Content, FileDataPart, FunctionResponseData, InlineDataPart, MAX_INLINE_DATA_SIZE, Part,
};

// Re-export async_trait so the #[tool] macro's generated code can reference it
// via adk_tool::async_trait (adk_tool re-exports from adk_core).
pub use async_trait::async_trait;

/// Enforces the explicit cryptographic provider process-wide.
/// This bypasses any inert 'ring' code lingering in deep dependencies.
///
/// Because we lack a monolithic main() function, we use lazy evaluation
/// to guarantee that aws-lc-rs is installed globally the millisecond
/// an adk-rust consumer attempts to use the network.
pub fn ensure_crypto_provider() {
    #[cfg(feature = "rustls")]
    {
        static CRYPTO_INIT: std::sync::Once = std::sync::Once::new();
        CRYPTO_INIT.call_once(|| {
            // We ignore the Result. If the parent application has already
            // deliberately installed a provider, we respect their sovereign choice.
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        });
    }
}
