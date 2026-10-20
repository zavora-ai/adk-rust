//! # adk-harness
//!
//! The governed, durable harness agent of ADK-Rust (spec 3.1,
//! `docs/design/spec-3.1-harness-agent.md`).
//!
//! A harness is the ready-made agent a developer runs first: file and shell tools, a
//! planning loop, skills, approvals and resumable sessions. `adk-harness` builds it as
//! configuration and policy over an `LlmAgent` (spec 3.1 §6 D-1): every tool call passes
//! the governed path, where [`HarnessPolicy`] applies the session's [`HarnessMode`] and the
//! subject's [`StandingRule`]s.
//!
//! | Item | Spec |
//! |---|---|
//! | [`HarnessAgentBuilder`], [`HarnessAgent`], [`Harness`] | §7.2, §7.3 |
//! | [`HarnessConfig`] (`.adk/harness.toml`) | §8.3 |
//! | [`HarnessMode`], [`ModeStore`], [`ExitPlanModeTool`] | §7.4 |
//! | [`StandingRule`], [`StandingRuleStore`], [`PromptingConfirmationHandler`] | §7.5 |
//! | [`HarnessPolicy`], [`ToolKind`], [`MODE_TABLE`] | §7.4, §9 |
//! | [`TodoList`], [`WriteTodosTool`], [`UpdateTodoTool`] | §7.6 |
//! | [`InstructionsConfig`], [`ProjectInstructions`], [`WorkspaceTrustStore`] | §7.7 |
//! | `SkillsConfig` (feature `skills`) | §7.8 |
//! | [`ShellIsolation`], [`HarnessBashTool`], [`ShellLedger`] | §7.9 |
//! | `SpendReport`, `LoopGuard`, `harness.*` span attributes (feature `observe`) | §7.13 |
//! | `RbacMap`, `ShellCredential`, `EphemeralAuditLog` (feature `auth`) | §7.14 |
//! | `MemoryConfig`, `FileMemory`, `memory_*` tools (feature `memory`) | §7.15 |
//! | `MemberSpec`, `HarnessTeam` (feature `team`) | §7.10 |
//! | `PlanGraphs`, `compile_plan` (feature `graph`), [`GoalSpec`], [`GoalStore`], [`UntilCheckAgent`] | §7.11 |
//! | `MemoryFact`, `FactStore`, `FactIndex`, `LongTermMemoryConfig`, `memory_search`, `memory_remember` (feature `memory`) | §7.15.2 |
//! | `hooks::HookSpec`, `hooks::HookSet`, `hooks::HookRunner` (feature `hooks`) | §7.22 |
//! | [`coding::CodingAgent`] | §7.19 |
//! | [`WebConfig`], [`SearchProvider`], [`ProviderWebTools`], `web_fetch`, `web_search` (feature `web`) | §7.20 |
//! | [`McpInteractionConfig`], `HarnessElicitationHandler`, `HarnessSamplingHandler`, `mcp_*_resource` (feature `mcp`) | §7.21 |
//! | [`EffortPolicy`], [`Effort`] | §7.23 |
//!
//! # Example
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use adk_harness::{HarnessAgent, HarnessMode};
//!
//! let harness = HarnessAgent::builder("harness")
//!     .model(model)
//!     .workspace(adk_devtools::Workspace::new(".").allow_bash(true))
//!     .mode(HarnessMode::Ask)
//!     .prompter(Arc::new(my_terminal_prompter))
//!     .build()?
//!     .into_harness("adk-rust", sessions)?;
//! let session = harness.open_session(&user, None).await?;
//! let events = harness.send(&user, &session, "make the failing test pass").await?;
//! ```

#![warn(missing_docs)]

mod agent;
mod audit;
mod builder;
mod config;
#[cfg(feature = "memory")]
mod facts;
mod fsutil;
mod goal;
#[cfg(feature = "runner")]
mod harness;
#[cfg(feature = "auth")]
mod identity;
mod instructions;
mod ledger;
pub mod mcp;
#[cfg(feature = "memory")]
mod memory;
mod mode;
#[cfg(feature = "observe")]
mod observe;
#[cfg(feature = "graph")]
mod plan_graph;
mod policy;
mod posture;
mod prompt;
mod run_context;
mod shell;
#[cfg(feature = "skills")]
mod skills;
#[cfg(feature = "team")]
mod team;
mod tools;
mod toolset;
pub mod web;

pub mod approvals;
#[path = "presets/coding.rs"]
pub mod coding;
pub mod conformance;
#[cfg(feature = "hooks")]
pub mod hooks;

pub use agent::{HARNESS_UNGOVERNED_CODE, HarnessAgent, HarnessInfo, HarnessLayer, HarnessRuntime};
#[cfg(feature = "sqlite")]
pub use approvals::SqliteStandingRuleStore;
pub use approvals::{
    ApprovalPrompt, ApprovalPrompter, ArgSpec, DEFAULT_PROMPTS_PER_TURN, FileStandingRuleStore,
    InMemoryStandingRuleStore, OriginPin, PromptAnswer, PromptingConfirmationHandler,
    RENDERED_CALL_MAX_BYTES, RenderedCall, RuleOwner, RulePredicate, RuleScope, RuleSuggestion,
    SHELL_PREFIX_SUGGESTION_DENYLIST, StandingDecision, StandingRule, StandingRuleStore,
    SuggestedScope, escape_control,
};
#[cfg(feature = "observe")]
pub use builder::LoopGuard;
pub use builder::{HarnessAgentBuilder, Pack};
pub use config::{HARNESS_CONFIG_FILE, HarnessConfig};
#[cfg(feature = "memory-embeddings")]
pub use facts::RagEmbedder;
#[cfg(all(feature = "memory", feature = "sqlite"))]
pub use facts::SqliteFactStore;
#[cfg(feature = "memory")]
pub use facts::{
    FACT_AUTHOR_PREFIX, FACT_TEXT_MAX_CHARS, FactFilter, FactIndex, FactKind, FactProjects,
    FactScope, FactSource, FactStatus, FactStore, FileFactStore, InMemoryFactStore, LocalFactIndex,
    LongTermMemoryConfig, MemoryFact, MemoryServiceIndex, Provenance, fact_digest,
    redact_fact_text,
};
#[cfg(feature = "graph")]
pub use goal::GoalEngine;
#[cfg(feature = "sqlite")]
pub use goal::SqliteGoalStore;
pub use goal::{
    GOAL_QUEUE, GOAL_STATE_KEY, GOAL_TARGET, GoalRecord, GoalSpec, GoalState, GoalStatus,
    GoalStore, InMemoryGoalStore, UNTIL_CHECK_AGENT, UntilCheckAgent,
};
#[cfg(feature = "runner")]
pub use harness::{CANCELLED_BY_MODE_CHANGE_STATUS, Harness, HarnessDecision};
#[cfg(feature = "auth")]
pub use identity::{EphemeralAuditLog, HARNESS_AUDIT_REQUIRED_CODE, RbacMap, ShellCredential};
pub use instructions::{
    FALLBACK_INSTRUCTION_FILES, FileWorkspaceTrustStore, InMemoryWorkspaceTrustStore,
    InstructionFile, InstructionScope, InstructionsConfig, ProjectInstructions, TrustDecision,
    WorkspaceTrust, WorkspaceTrustStore,
};
pub use mcp::McpInteractionConfig;
#[cfg(feature = "mcp")]
pub use mcp::{
    HarnessElicitationHandler, HarnessSamplingHandler, McpInteractionRuntime, McpServerSet,
    RESOURCE_TEXT_MAX, defaults_of, render_form, validate_answer,
};
#[cfg(feature = "memory")]
pub use memory::{
    FileMemory, MEMORY_INDEX_FILE, MEMORY_INDEX_MARKER, MemoryChange, MemoryConfig, MemoryLayout,
    MemoryLimits, MemoryTopic, MemoryWritePolicy, project_slug,
};
#[cfg(feature = "sqlite")]
pub use mode::SqliteModeStore;
pub use mode::{
    FileModeStore, HarnessMode, InMemoryModeStore, MODE_STATE_KEY, ModeChange, ModeRecord,
    ModeSource, ModeStore,
};
#[cfg(feature = "observe")]
pub use observe::{
    ATTR_GOAL_ITERATION, ATTR_ISOLATION, ATTR_MEMBER, ATTR_MODE, ATTR_RULE_ID, ATTR_TOOL_KIND,
    LOOP_DETECTED_CODE, SpendEntry, SpendReport, TurnTokens, goal_iteration_span, isolation_label,
    member_span, run_span, tool_kind_label,
};
#[cfg(feature = "graph")]
pub use plan_graph::{
    ActivePlan, PLAN_GATE_TOOL, PlanGraphShape, PlanGraphs, PlanRunStatus, compile_plan,
    plan_review_node, plan_step_node,
};
pub use policy::{
    HARNESS_POLICY_REF, HarnessPolicy, Isolation, MODE_TABLE, ModeCell, PolicySwitches,
    TableDecision, TableRow, ToolClassRegistry, ToolKind, apply_rule, decision_cell,
    has_harness_layer,
};
pub use posture::{Effort, EffortPolicy};
pub use prompt::HARNESS_SYSTEM_PROMPT;
#[cfg(feature = "sqlite")]
pub use shell::SqliteShellLedger;
pub use shell::{AUTO_ALLOW_UNAVAILABLE_CODE, SANDBOX_PROTECTED_PATHS};
pub use shell::{
    InMemoryShellLedger, IsolationStatus, SANDBOX_UNAVAILABLE_CODE, ShellConfig, ShellIsolation,
    ShellLedger, ShellLedgerEntry,
};
#[cfg(feature = "sandbox")]
pub use shell::{OsSandboxConfig, SandboxAccess, SandboxLauncher};
#[cfg(feature = "skills")]
pub use skills::{SkillsConfig, SkillsMode};
#[cfg(feature = "team")]
pub use team::{
    HarnessTeam, MEMBER_CIRCUIT_BREAKER, MEMBER_MAX_ITERATIONS, MEMBER_TIMEOUT, MemberKind,
    MemberSpec, MemberTools, harness_team_policy,
};
pub use tools::{
    ExitPlanModeTool, GOVERNANCE_POLL_INTERVAL, HarnessBashTool, MAX_TODO_CHARS, MAX_TODO_ITEMS,
    PLAN_STATE_KEY, PlanGate, PlanStep, TODOS_STATE_KEY, TodoItem, TodoList, TodoStatus,
    UpdateTodoTool, WriteTodosTool,
};
#[cfg(feature = "mcp")]
pub use tools::{McpListResourcesTool, McpReadResourceTool};
#[cfg(feature = "memory")]
pub use tools::{MemoryDeleteTool, MemoryReadTool, MemoryWriteTool};
#[cfg(feature = "memory")]
pub use tools::{MemoryRememberTool, MemorySearchTool};
#[cfg(feature = "web")]
pub use tools::{WEB_TURN_CAP_CODE, WebFetchTool, WebSearchTool};
pub use toolset::HarnessToolset;
pub use web::{ProviderWebTools, SearchHit, SearchProvider, WebConfig};
