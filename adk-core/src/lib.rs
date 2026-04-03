//! # adk-core
//!
//! Core traits and types for ADK agents, tools, sessions, and events.
#![allow(clippy::result_large_err)]
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

pub mod agent;
pub mod agent_loader;
pub mod callbacks;
pub mod context;
pub mod error;
pub mod event;
pub mod identity;
pub mod instruction_template;
pub mod model;
pub mod request_context;
pub mod tool;
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
    Artifacts, CallbackContext, IncludeContents, InvocationContext, MAX_STATE_KEY_LEN, Memory,
    MemoryEntry, ReadonlyContext, ReadonlyState, RunConfig, Session, State, StreamingMode,
    ToolConfirmationDecision, ToolConfirmationPolicy, ToolConfirmationRequest, ToolOutcome,
    validate_state_key,
};
pub use error::{AdkError, ErrorCategory, ErrorComponent, ErrorDetails, Result, RetryHint};
pub use event::{
    Event, EventActions, EventCompaction, KEY_PREFIX_APP, KEY_PREFIX_TEMP, KEY_PREFIX_USER,
};
pub use identity::{
    AdkIdentity, AppName, ExecutionIdentity, IdentityError, InvocationId, SessionId, UserId,
};
pub use instruction_template::inject_session_state;
pub use model::{
    CacheCapable, CitationMetadata, CitationSource, ContextCacheConfig, FinishReason,
    GenerateContentConfig, Llm, LlmRequest, LlmResponse, LlmResponseStream, UsageMetadata,
};
pub use request_context::RequestContext;
pub use tool::{
    RetryBudget, Tool, ToolContext, ToolExecutionStrategy, ToolPredicate, ToolRegistry, Toolset,
    ValidationMode,
};
pub use types::{Content, FunctionResponseData, MAX_INLINE_DATA_SIZE, Part};

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
