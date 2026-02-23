//! # adk-core
//!
//! Core traits and types for ADK agents, tools, sessions, and events.
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
pub mod instruction_template;
pub mod model;
pub mod tool;
pub mod types;

pub use agent::{Agent, EventStream, ResolvedContext};
pub use agent_loader::{AgentLoader, MultiAgentLoader, SingleAgentLoader};
pub use callbacks::{
    AfterAgentCallback, AfterModelCallback, AfterToolCallback, BaseEventsSummarizer,
    BeforeAgentCallback, BeforeModelCallback, BeforeModelResult, BeforeToolCallback,
    EventsCompactionConfig, GlobalInstructionProvider, InstructionProvider,
};
pub use context::{
    Artifacts, CallbackContext, IncludeContents, InvocationContext, MAX_STATE_KEY_LEN, Memory,
    MemoryEntry, ReadonlyContext, ReadonlyState, RunConfig, Session, State, StreamingMode,
    ToolConfirmationDecision, ToolConfirmationPolicy, ToolConfirmationRequest, validate_state_key,
};
pub use error::{AdkError, Result};
pub use event::{
    Event, EventActions, EventCompaction, KEY_PREFIX_APP, KEY_PREFIX_TEMP, KEY_PREFIX_USER,
};
pub use instruction_template::inject_session_state;
pub use model::{
    CacheCapable, CitationMetadata, CitationSource, ContextCacheConfig, FinishReason,
    GenerateContentConfig, Llm, LlmRequest, LlmResponse, LlmResponseStream, UsageMetadata,
};
pub use tool::{Tool, ToolContext, ToolPredicate, ToolRegistry, Toolset, ValidationMode};
pub use types::{Content, FunctionResponseData, MAX_INLINE_DATA_SIZE, Part};
