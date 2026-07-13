//! First-party ADK-Rust orchestration contracts for `computer-use-mcp` v8.

mod auth;
mod cancellation;
mod contracts;
mod eval;
mod graph;
mod mcp_runtime;

pub use auth::{ComputerUseAuthContext, ScopeAuthorizer};
pub use cancellation::{AgentInterrupter, CancellationBridge, CancellationError};
pub use contracts::*;
pub use eval::{
    AdkEvaluationClaims, AdkEvaluationReceipt, AdkEvaluationSource, ComputerUseEvaluation,
    ComputerUseEvaluator,
};
pub use graph::{
    ComputerUseRuntime, build_reference_graph, build_reference_graph_with_checkpointer,
};
pub use mcp_runtime::{ComputerUseMcpConfig, ComputerUseMcpRuntime, TraceCorrelation};
