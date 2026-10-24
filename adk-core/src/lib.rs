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
//! - [`Llm`] - The interface every model provider implements
//! - [`Session`] / [`State`] - For managing conversation context
//! - [`Event`] - For streaming agent responses
//! - [`AdkError`] / [`Result`] - Unified error handling
//! - [`SharedState`] / [`SharedStateError`] - Thread-safe key-value store for parallel agent coordination
//! - [`ToolConfirmationPolicy`] / [`ToolConfirmationRequest`] - Human-in-the-loop tool authorization
//! - [`ToolPolicy`] / [`DeclarativePolicy`] / [`authorize_tool_call`] - The governed tool execution path
//! - [`GovernanceControl`] - Organisation-wide kill switch
//! - [`RunBudget`] / [`SpendLedger`] / [`ActionLedger`] - Run limits, spend caps, and at-most-once tool calls
//! - [`AgentId`] / [`AgentPrincipal`] / [`ActingContext`] / [`ActingConfig`] - Agent identity
//! - [`LeaseStore`] / [`DurableRunStore`] / [`ScheduleStore`] / [`GovernanceStateStore`] -
//!   Durable runtime contracts: fenced leases, the run queue, schedules, and freeze state
//!
//! ## Feature Flags
//!
//! | Feature | Enables |
//! |---------|---------|
//! | `rustls` | [`ensure_crypto_provider`] installs the `aws-lc-rs` rustls provider |
//! | `conformance` | The `conformance` module: suites for durable store backends |
//! | `cron` | Cron schedules in IANA time zones |
//! | `webhooks` | The `webhook` module: Standard Webhooks signing and verification |
//! | `webhook-delivery` | The `webhook_delivery` module: signed outbound delivery |
//!
//! ## Quick Start
//!
//! ```rust
//! use std::sync::Arc;
//!
//! use adk_core::{Result, Tool, ToolContext, ToolEffect};
//! use async_trait::async_trait;
//! use serde_json::{Value, json};
//!
//! struct Echo;
//!
//! #[async_trait]
//! impl Tool for Echo {
//!     fn name(&self) -> &str {
//!         "echo"
//!     }
//!
//!     fn description(&self) -> &str {
//!         "Returns its arguments"
//!     }
//!
//!     fn effect(&self) -> ToolEffect {
//!         ToolEffect::ReadOnly
//!     }
//!
//!     async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
//!         Ok(json!({ "echo": args }))
//!     }
//! }
//!
//! assert_eq!(Echo.effect(), ToolEffect::ReadOnly);
//! ```
//!
//! ## Core Traits
//!
//! ### Agent
//!
//! The [`Agent`] trait defines the interface for all agents. `name`, `description`, `sub_agents`,
//! and `run` are required; `description` returns `&str`, and a leaf agent returns `&[]` from
//! `sub_agents`.
//!
//! ```rust
//! use std::sync::Arc;
//!
//! use adk_core::{Agent, EventStream, InvocationContext, Result};
//! use async_trait::async_trait;
//!
//! struct Silent;
//!
//! #[async_trait]
//! impl Agent for Silent {
//!     fn name(&self) -> &str {
//!         "silent"
//!     }
//!
//!     fn description(&self) -> &str {
//!         "Produces no events"
//!     }
//!
//!     fn sub_agents(&self) -> &[Arc<dyn Agent>] {
//!         &[]
//!     }
//!
//!     async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
//!         Ok(Box::pin(futures::stream::empty()))
//!     }
//! }
//! ```
//!
//! ### Tool
//!
//! The [`Tool`] trait defines custom capabilities. [`Tool::effect`] declares whether a call is
//! read-only, idempotent, or non-idempotent, which governs retries and the action ledger.
//!
//! ## State Management
//!
//! State uses typed prefixes for organization:
//!
//! - `user:` - User preferences (persists across sessions)
//! - `app:` - Application state (application-wide)
//! - `temp:` - Visible for the rest of the invocation; session services do not persist it
#![allow(clippy::result_large_err)]
#![deny(missing_docs)]

/// How a run's agents act: the identity model, the run-level acting configuration,
/// delegation, and subject delegation policies.
pub mod acting;
/// Durable record of non-idempotent tool calls.
pub mod action_ledger;
/// Core agent trait and event stream type.
pub mod agent;
/// Starting an agent turn without owning the runner's construction.
pub mod agent_invoker;
/// Dynamic agent loading by name.
pub mod agent_loader;
/// Approval service: an inbox of approval requests with quorum, delegation, escalation,
/// and expiry over an approval store.
pub mod approval;
/// Append-only audit log records and the store that holds them.
pub mod audit_log;
/// Run budgets: limits on model calls, tokens, cost, wall time, and tool calls.
pub mod budget;
/// Callback type aliases for agent, model, and tool lifecycle hooks.
pub mod callbacks;
/// Time sources for stores whose behaviour depends on the current time.
pub mod clock;
/// Conformance suites that every durable store backend runs against its implementation.
#[cfg(feature = "conformance")]
pub mod conformance;
/// Invocation context traits: state, session, artifacts, memory, and run configuration.
pub mod context;
/// Agent credentials, identity sources, and resource-scoped token brokers.
pub mod credential;
/// Unified structured error type and result alias.
pub mod error;
/// Event types representing agent interactions in a conversation.
pub mod event;
mod event_stream;
/// Governed tool execution: kill switch, durable approvals, and the authorization path.
pub mod governance;
/// Durable freeze state for the kill switch, shared by every replica.
pub mod governance_state;
/// Building model history from session events: other agents' turns as context, tool
/// branches, and balanced history cuts.
pub mod history;
/// Typed identity primitives for app, user, session, and invocation.
pub mod identity;
/// Template-based instruction injection with session state interpolation.
pub mod instruction_template;
/// Intra-turn context compaction configuration.
pub mod intra_compaction;
/// Leases with fencing epochs: one owner of a named resource at a time.
pub mod lease;
/// Metering for every model call made on behalf of a run.
pub mod metering;
/// LLM trait, request/response types, and caching configuration.
pub mod model;
/// Tool execution policy: allow, deny, or require approval per call.
pub mod policy;
/// Agent identity: agent identifiers, principals, and acting contexts.
pub mod principal;
pub mod queue_source;
/// HTTP request context extracted by auth middleware.
pub mod request_context;
/// Durable runs: a queue of runs that workers claim under fenced leases.
pub mod run_store;
/// Schedules that enqueue durable runs, and the firing rule.
pub mod schedule;
/// Provider-aware JSON Schema normalization for tool declarations.
pub mod schema_adapter;
/// Thread-safe schema cache for tool parameter schemas.
pub mod schema_cache;
/// JSON Schema utility functions.
pub mod schema_utils;
/// Thread-safe shared state for parallel agent coordination.
pub mod shared_state;
/// One spend ledger for model calls and payments: reservations, limits, and committed spend.
pub mod spend;
/// Tool trait, toolset, execution strategy, and registry.
pub mod tool;
/// Semaphore-based tool concurrency management.
pub mod tool_concurrency;
/// Function responses for tool calls that fail, are withheld, or carry undecodable arguments.
pub mod tool_response;
/// Content, Part, and multimodal data types.
pub mod types;
#[cfg(feature = "webhooks")]
pub mod webhook;
#[cfg(feature = "webhook-delivery")]
pub mod webhook_delivery;

pub use acting::{
    ActingConfig, PRINCIPAL_NOT_ALLOWED_CODE, SUBJECT_MISMATCH_CODE, StaticSubjectDelegation,
    SubjectDelegationPolicy, SubjectDelegationRequest, subject_mismatch_error,
};
pub use action_ledger::{
    ActionLedger, ActionOutcome, ActionRecord, InMemoryActionLedger, OUTCOME_UNKNOWN_STATUS,
    is_outcome_unknown, json_digest, outcome_unknown_response,
};
pub use agent::{
    Agent, AgentCapabilities, AgentInteractionMode, AgentRelationshipKind, AgentTopology,
    AgentTopologyMember, AgentTopologyRelationship, AgentTransferDecision, AgentTransferRequest,
    EventStream, ResolvedContext, screen_user_input_with,
};
pub use agent_invoker::AgentInvoker;
pub use agent_loader::{AgentLoader, MultiAgentLoader, SingleAgentLoader};
pub use approval::{
    APPROVAL_REAPPLY_GRACE, ApprovalAction, ApprovalCall, ApprovalCursor, ApprovalDecision,
    ApprovalDelegation, ApprovalEventKind, ApprovalNotification, ApprovalNotifier, ApprovalPolicy,
    ApprovalRequest, ApprovalRequestQuery, ApprovalRisk, ApprovalRoute, ApprovalRouter,
    ApprovalService, ApprovalState, ApprovalTick, ApproverSelector, DEFAULT_APPROVAL_PAGE,
    DelegationScope, DelegationWindow, EscalationStep, ExpiryAction, InboxPage, InboxQuery,
    StoreApprovalService,
};
pub use budget::{
    BUDGET_LIMIT_KEY, BUDGET_RECORDED_KEY, BudgetExceeded, BudgetLimit, BudgetTracker, BudgetUsage,
    ModelCallMeter, RunBudget, budget_exceeded_event, generate_with_budget, meter_stream,
};
pub use callbacks::{
    AfterAgentCallback, AfterModelCallback, AfterToolCallback, AfterToolCallbackFull,
    BaseEventsSummarizer, BeforeAgentCallback, BeforeModelCallback, BeforeModelResult,
    BeforeToolCallback, EventsCompactionConfig, GlobalInstructionProvider, InstructionProvider,
    InvocationHooks, OnToolErrorCallback,
};
pub use clock::{Clock, ManualClock, SystemClock};
pub use context::{
    Artifacts, BUILTIN_TOOL_ORIGIN, BackpressurePolicy, CallbackContext, IncludeContents,
    InvocationContext, MAX_STATE_KEY_LEN, Memory, MemoryEntry, ReadonlyContext, ReadonlyState,
    RunConfig, RunConfigBuilder, RuntimeToolset, STATIC_TOOL_ORIGIN, SecretRequest, SecretService,
    Session, State, StreamingMode, ToolCallbackContext, ToolConcurrencyConfig,
    ToolConfirmationDecision, ToolConfirmationHandler, ToolConfirmationPolicy,
    ToolConfirmationRequest, ToolOrigin, ToolOutcome, tool_call_fingerprint, validate_state_key,
};
pub use credential::{
    AgentCredential, CredentialBroker, CredentialKind, CredentialSecret, IdentitySource,
    RevocationScope, ScopedToken, StaticIdentitySource, resource_indicator,
};
pub use error::{
    AdkError, ErrorCategory, ErrorComponent, ErrorDetails, ErrorDiagnostics, Result, RetryHint,
    UPSTREAM_EXCERPT_BYTES, json_error_summary, upstream_excerpt,
};
pub use event::{
    Event, EventActions, EventCompaction, KEY_PREFIX_APP, KEY_PREFIX_TEMP, KEY_PREFIX_USER,
    TOOL_PROGRESS_CALL_ID_KEY, TOOL_PROGRESS_STREAM_KEY, ToolCallView, ToolResultView,
    WorkflowPosition, event_belongs_to_branch,
};
pub use event_stream::EventTextDeltas;
pub use governance::{
    ApprovalScope, ApprovalStore, BUILTIN_TOOLS_WITHHELD_KEY, BuiltinToolGovernor,
    BuiltinToolWithholding, CONFIRMATION_PENDING_STATUS, DEFAULT_TOOL_CONFIRMATION_TIMEOUT,
    GOVERNANCE_FROZEN_CODE, GOVERNANCE_STALE_CODE, GovernanceControl, GovernanceMirrorConfig,
    GovernanceWatch, GovernedCall, HeldToolCall, InMemoryApprovalStore, PendingApproval,
    ToolApproval, ToolAuthorization, ToolCallScreen, ToolGate, WithheldBuiltinTool,
    authorize_tool_call, builtin_tool_configuration, confirm_tool_call,
    confirmation_decision_available, confirmation_pending_response, held_call_compaction_limit,
    held_tool_calls, is_confirmation_pending, supersede_pending_responses, withdraw_held_call,
    workflow_resume_positions,
};
pub use governance_state::{
    GovernanceScope, GovernanceState, GovernanceStateStore, InMemoryGovernanceStateStore,
};
pub use identity::{
    AdkIdentity, AppName, ExecutionIdentity, IdentityError, InvocationId, SessionId, UserId,
};
pub use instruction_template::inject_session_state;
pub use intra_compaction::IntraCompactionConfig;
pub use lease::{InMemoryLeaseStore, Lease, LeaseStore};
pub use metering::{
    LlmSpendEstimate, MeteredLlm, ModelCallScope, ModelSpendHold, UNBOUNDED_HOLD_MICRO_USD,
    generate_metered, vendor_for_model,
};
pub use model::{
    BuiltinToolUse, CacheCapable, CitationMetadata, CitationSource, ContextCacheConfig, Effort,
    FinishReason, GenerateContentConfig, Llm, LlmRequest, LlmResponse, LlmResponseStream,
    UsageMetadata, nullable_count,
};
pub use model::{NativeWebSearch, NativeWebSearchTool};
pub use policy::{
    ArgPredicate, DeclarativePolicy, DeclarativePolicyBuilder, PolicyCall, PolicyDecision,
    PolicyRule, StrictestPolicy, ToolPolicy, ToolPolicyRequest,
};
pub use principal::{
    ActingContext, AgentId, AgentPrincipal, PrincipalError, SubjectKind, SubjectRef,
};
pub use queue_source::{
    InMemoryQueueSource, IngressReport, QueueIngress, QueueMessage, QueueSource,
};
pub use request_context::RequestContext;
pub use run_store::{
    ClaimedRun, DurableRunStore, Enqueued, InMemoryRunStore, NewRun, OnConflict, RunCheckpointRef,
    RunFilter, RunOutcome, RunRecord, RunState,
};
pub use schedule::{
    FirePlan, InMemoryScheduleStore, MissedFirePolicy, Schedule, ScheduleSpec, ScheduleStore,
    firing_idempotency_key, plan_fires,
};
pub use schema_adapter::{GenericSchemaAdapter, SchemaAdapter};
pub use schema_cache::SchemaCache;
pub use shared_state::{SharedState, SharedStateError};
pub use spend::{
    DEFAULT_RESERVATION_TTL, InMemorySpendLedger, ReservationId, SPEND_LIMIT_EXCEEDED_CODE,
    SpendError, SpendKey, SpendLedger, SpendLimit, SpendLimits, SpendPeriod, usd_to_micro_usd,
};
pub use tool::{
    DUPLICATE_TOOL_NAME_CODE, RetryBudget, Tool, ToolContext, ToolEffect, ToolExecutionStrategy,
    ToolPredicate, ToolRegistry, Toolset, ValidationMode, ensure_unique_tool_names,
};
pub use tool_concurrency::{ConcurrencyPermit, ToolConcurrencyManager};
pub use tool_response::{
    INVALID_TOOL_ARGUMENTS_KEY, RESULT_WITHHELD_STATUS, TOOL_ERROR_MESSAGE_LIMIT,
    TOOL_INVALID_ARGUMENTS_CODE, TOOL_PERMISSION_DENIED_CODE, ToolResultScreen,
    invalid_tool_arguments, invalid_tool_arguments_reason, result_blocked_response,
    result_withheld_response, screen_tool_result, tool_error_response,
};
pub use types::{
    BlobResourceContents, Content, EmbeddedResource, FileDataPart, FunctionResponseData,
    GENERATED_CALL_ID_PREFIX, InlineDataPart, MAX_INLINE_DATA_SIZE, Part, TextResourceContents,
};
pub use url::Url;

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
