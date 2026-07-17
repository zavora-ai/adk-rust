//! # adk-computer-use
//!
//! First-party ADK-Rust orchestration and wire contracts for the
//! [`computer-use-mcp`](https://github.com/zavora-ai/computer-use-mcp)
//! desktop-automation server.
//!
//! This crate **does not** perform desktop actuation. Desktop policy, target
//! validation, lease ownership, physical-user interruption, and idempotent
//! effects remain authoritative in `computer-use-mcp`. Instead, this crate
//! supplies the governed ADK-side layer that drives that server safely:
//!
//! - **Wire contracts** ([`contracts`]) — camelCase wire types with disclosure-safe
//!   validation (digest-only postconditions, value-free sensitivity evidence,
//!   bounded approval scopes).
//! - **Authorization** ([`ScopeAuthorizer`], [`ComputerUseAuthContext`]) — a coarse
//!   `computer:*` scope gate bound to an [`adk_auth`]-verified identity that
//!   model or graph state cannot forge.
//! - **Deterministic workflow** ([`build_reference_graph`]) — an [`adk_graph`]
//!   graph that fans observation out in parallel, previews before mutation,
//!   interrupts for digest-bound approval, executes exactly once, and verifies
//!   the receipt.
//! - **Runtime boundary** ([`ComputerUseRuntime`]) — the trait the graph drives,
//!   with a live MCP adapter ([`ComputerUseMcpRuntime`]) or an in-process fake.
//! - **Cancellation** ([`CancellationBridge`]) — revokes desktop authority
//!   before stopping ADK reasoning.
//! - **Release evaluation** ([`ComputerUseEvaluator`], [`AdkEvaluationReceipt`]) —
//!   deterministic trajectory scoring and a tamper-evident evidence receipt.
//!
//! ## Quick start
//!
//! The graph is driven through the [`ComputerUseRuntime`] trait, so it can run
//! against an in-process implementation with no external server. See the
//! `minimal_graph` example for a complete, runnable version:
//!
//! ```no_run
//! use std::sync::Arc;
//! use adk_computer_use::{build_reference_graph, ScopeAuthorizer};
//! # use adk_computer_use::ComputerUseRuntime;
//! # fn build(runtime: Arc<dyn ComputerUseRuntime>) -> Result<(), adk_graph::GraphError> {
//! let authorizer = Arc::new(ScopeAuthorizer::new(["computer:plan", "computer:execute:background"]));
//! let graph = build_reference_graph(runtime, authorizer)?;
//! # let _ = graph;
//! # Ok(())
//! # }
//! ```
//!
//! To drive a real desktop, back the graph with [`ComputerUseMcpRuntime`] and a
//! running `computer-use-mcp` server (see the macOS examples).

mod auth;
mod cancellation;
pub mod contracts;
mod error;
mod eval;
mod graph;
mod runtime;

pub use auth::{AuthorizationError, ComputerUseAuthContext, ScopeAuthorizer};
pub use cancellation::{AgentInterrupter, CancellationBridge, CancellationError};
pub use error::{ComputerUseError, Result};
pub use eval::{
    AdkEvaluationClaims, AdkEvaluationReceipt, AdkEvaluationSource, ComputerUseEvaluation,
    ComputerUseEvaluator,
};
pub use graph::{build_reference_graph, build_reference_graph_with_checkpointer};
pub use runtime::{
    ComputerUseMcpConfig, ComputerUseMcpRuntime, ComputerUseRuntime, TraceCorrelation,
};

// Wire contracts are re-exported at the crate root for ergonomic access.
pub use contracts::{
    ActionClass, ActionEnvelope, ActionPostcondition, ActionPreview, ActionProvenance,
    ActionResourceContext, ApprovalGrant, ApprovalGrantScope, Bounds, ControlLease,
    ExecutionCapability, ExecutionMode, ExecutionReceipt, LeaseBoundaries, PolicyDecision,
    PostconditionEvidence, ReceiptStatus, RuntimeSession, SafetyCorpus, SafetyExpectation,
    SafetyScenario, SessionCompletionEvidence, SessionDeletionResult, SessionEvent,
    SessionFollowUp, SessionFollowUpPage, TargetEvidence, TargetReservation,
    TargetReservationScope, TargetSensitivityAssessment, TargetSensitivityEvidence,
    TargetSensitivitySignal, TargetSensitivitySource,
};
