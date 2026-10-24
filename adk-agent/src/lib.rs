//! # adk-agent
//!
//! Agent implementations for ADK (LLM, Custom, Workflow agents).
//!
//! ## Overview
//!
//! This crate provides ready-to-use agent implementations:
//!
//! - [`LlmAgent`] - Core agent powered by LLM reasoning
//! - [`CustomAgent`] - Define custom logic without LLM
//! - [`SequentialAgent`] - Execute agents in sequence
//! - [`ParallelAgent`] - Execute agents concurrently, with optional [`SharedState`](adk_core::SharedState) coordination
//! - [`LoopAgent`] - Iterate until a sub-agent escalates or the iteration limit is reached
//! - [`ConditionalAgent`] - Branch based on conditions
//! - [`LlmConditionalAgent`] - Route to a sub-agent by a model classification
//! - [`TeamSpec`] - Validate and compile a portable team from existing agents
//!
//! `CodeActAgent` (feature `codeact`), `CodingAgent` (feature `coding`), and `AmbientAgent`
//! (feature `ambient`) are feature-gated.
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use std::sync::Arc;
//!
//! use adk_agent::LlmAgentBuilder;
//! use adk_model::GeminiModel;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let api_key = std::env::var("GOOGLE_API_KEY")?;
//! let model = Arc::new(GeminiModel::new(&api_key, "gemini-3.7-flash")?);
//!
//! let agent = LlmAgentBuilder::new("assistant")
//!     .description("Helpful AI assistant")
//!     .instruction("Be helpful and concise.")
//!     .model(model)
//!     .build()?;
//! # let _ = agent;
//! # Ok(())
//! # }
//! ```
//!
//! ## Workflow Agents
//!
//! Combine agents for complex workflows:
//!
//! ```rust
//! use std::sync::Arc;
//!
//! use adk_agent::{LoopAgent, ParallelAgent, SequentialAgent};
//! use adk_core::Agent;
//!
//! fn workflows(a: Arc<dyn Agent>, b: Arc<dyn Agent>, c: Arc<dyn Agent>) {
//!     // Sequential: A -> B -> C
//!     let _seq = SequentialAgent::new("pipeline", vec![a.clone(), b.clone(), c.clone()]);
//!
//!     // Parallel: A, B, C simultaneously
//!     let _par = ParallelAgent::new("team", vec![a, b, c.clone()]);
//!
//!     // Loop: repeat until a sub-agent escalates
//!     let _loop_agent = LoopAgent::new("iterator", vec![c]).with_max_iterations(10);
//! }
//! ```
//!
//! ## Guardrails (optional)
//!
//! Enable the `guardrails` feature for input, output, and tool guardrails:
//!
//! ```rust,ignore
//! use adk_agent::{LlmAgentBuilder, guardrails::{ContentFilter, GuardrailSet, PiiRedactor}};
//!
//! let input_guardrails = GuardrailSet::new()
//!     .with(ContentFilter::harmful_content())
//!     .with(PiiRedactor::new());
//!
//! let agent = LlmAgentBuilder::new("assistant")
//!     .model(model)
//!     .input_guardrails(input_guardrails)
//!     .build()?;
//! ```
#![allow(clippy::result_large_err)]
#![deny(missing_docs)]

mod acting_agent_context;
#[cfg(feature = "ambient")]
pub mod ambient;

#[cfg(feature = "coding")]
pub mod coding;

#[cfg(feature = "codeact")]
pub mod codeact;

pub mod compaction;
mod custom_agent;
pub mod guardrails;
mod invocation_hooks;
mod llm_agent;
mod skill_shim;
pub mod team;
pub mod tool_call_markup;
mod tool_ledger;
mod tool_response;
mod workflow;

pub use adk_core::AfterToolCallbackFull;
pub use adk_core::Agent;
pub use adk_core::OnToolErrorCallback;
pub use compaction::LlmEventSummarizer;
pub use custom_agent::{CustomAgent, CustomAgentBuilder};
pub use guardrails::GuardrailSet;
pub use llm_agent::{
    DEFAULT_MAX_ITERATIONS, DEFAULT_TOOL_TIMEOUT, LlmAgent, LlmAgentBuilder, extract_typed,
};
pub use team::{
    BlackboardHistoryPolicy, BlackboardPolicy, BlackboardSchedule, BlackboardSpec,
    BlackboardTransition, CircuitBreakerPolicy, CompiledBlackboardTeam, CompiledTeam,
    RelationshipApprovalPolicy, RelationshipFailureStrategy, RelationshipKind, RelationshipPolicy,
    ResolvedTeamMember, StaticTeamAgentRegistry, TEAM_EDGE_ID_KEY, TEAM_EXECUTION_STATE_KEY,
    TEAM_ROOT_INVOCATION_KEY, TeamAgentDescriptor, TeamAgentHealth, TeamAgentRegistry,
    TeamArchitectureTemplate, TeamBudget, TeamContextPolicy, TeamEdgeExecution, TeamError,
    TeamExecutionAnalysis, TeamExecutionSnapshot, TeamExecutionStatus, TeamExecutionUsage,
    TeamFailurePolicy, TeamHistoryPolicy, TeamLifecycleContext, TeamLifecycleDecision,
    TeamLifecycleHook, TeamLifecycleOutcome, TeamLifecyclePhase, TeamManagerBranch, TeamMemberSpec,
    TeamPolicy, TeamRegistryRequirement, TeamRelationship, TeamReplayError, TeamResumePlan,
    TeamResumePolicy, TeamRuntimeError, TeamSpec, TeamStateMergePolicy, TeamTerminationPolicy,
    WorkflowArchitectureTemplate, analyze_team_execution, validate_team_replay,
};
pub use tool_call_markup::{normalize_content, normalize_option_content};
pub use tool_ledger::held_call_ledger_key;
pub use workflow::{
    ConditionalAgent, DEFAULT_LOOP_MAX_ITERATIONS, LlmConditionalAgent, LlmConditionalAgentBuilder,
    LoopAgent, ParallelAgent, SequentialAgent,
};

#[cfg(feature = "ambient")]
pub use ambient::{
    AmbientAgent, AmbientAgentStatus, CronTrigger, EventSource, FileWatchTrigger, TriggerEvent,
    WebhookTrigger,
};
