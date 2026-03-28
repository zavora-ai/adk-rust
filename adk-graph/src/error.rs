//! Error types for adk-graph

use crate::interrupt::Interrupt;
use thiserror::Error;

/// Result type for graph operations
pub type Result<T> = std::result::Result<T, GraphError>;

/// Errors that can occur during graph operations
#[derive(Error, Debug)]
pub enum GraphError {
    /// Graph structure is invalid
    #[error("Invalid graph structure: {0}")]
    InvalidGraph(String),

    /// Node not found
    #[error("Node not found: {0}")]
    NodeNotFound(String),

    /// Edge target not found
    #[error("Edge target not found: {0}")]
    EdgeTargetNotFound(String),

    /// No entry point defined
    #[error("No entry point defined (missing edge from START)")]
    NoEntryPoint,

    /// Recursion limit exceeded
    #[error("Recursion limit exceeded: {0} steps")]
    RecursionLimitExceeded(usize),

    /// Execution was interrupted
    #[error("Execution interrupted: {0:?}")]
    Interrupted(Box<InterruptedExecution>),

    /// Node execution failed
    #[error("Node '{node}' execution failed: {message}")]
    NodeExecutionFailed { node: String, message: String },

    /// State serialization error
    #[error("State serialization error: {0}")]
    SerializationError(String),

    /// Checkpoint error
    #[error("Checkpoint error: {0}")]
    CheckpointError(String),

    /// Router returned unknown target
    #[error("Router returned unknown target: {0}")]
    UnknownRouteTarget(String),

    /// IO error
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    /// JSON error
    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),

    /// Database error (when sqlite feature enabled)
    #[cfg(feature = "sqlite")]
    #[error("Database error: {0}")]
    DatabaseError(#[from] sqlx::Error),
}

/// Information about an interrupted execution
#[derive(Debug, Clone)]
pub struct InterruptedExecution {
    /// Thread ID for resumption
    pub thread_id: String,
    /// Checkpoint ID for resumption
    pub checkpoint_id: String,
    /// The interrupt that occurred
    pub interrupt: Interrupt,
    /// Current state at interruption
    pub state: crate::state::State,
    /// Step number when interrupted
    pub step: usize,
}

impl InterruptedExecution {
    /// Create a new interrupted execution
    pub fn new(
        thread_id: String,
        checkpoint_id: String,
        interrupt: Interrupt,
        state: crate::state::State,
        step: usize,
    ) -> Self {
        Self { thread_id, checkpoint_id, interrupt, state, step }
    }
}

impl From<GraphError> for adk_core::AdkError {
    fn from(err: GraphError) -> Self {
        use adk_core::{ErrorCategory, ErrorComponent};
        let (category, code) = match &err {
            GraphError::InvalidGraph(_) => (ErrorCategory::InvalidInput, "graph.invalid"),
            GraphError::NodeNotFound(_) => (ErrorCategory::NotFound, "graph.node_not_found"),
            GraphError::EdgeTargetNotFound(_) => {
                (ErrorCategory::NotFound, "graph.edge_target_not_found")
            }
            GraphError::NoEntryPoint => (ErrorCategory::InvalidInput, "graph.no_entry_point"),
            GraphError::RecursionLimitExceeded(_) => {
                (ErrorCategory::Internal, "graph.recursion_limit")
            }
            GraphError::Interrupted(_) => (ErrorCategory::Cancelled, "graph.interrupted"),
            GraphError::NodeExecutionFailed { .. } => {
                (ErrorCategory::Internal, "graph.node_execution_failed")
            }
            GraphError::SerializationError(_) => (ErrorCategory::Internal, "graph.serialization"),
            GraphError::CheckpointError(_) => (ErrorCategory::Internal, "graph.checkpoint"),
            GraphError::UnknownRouteTarget(_) => {
                (ErrorCategory::NotFound, "graph.unknown_route_target")
            }
            GraphError::IoError(_) => (ErrorCategory::Internal, "graph.io"),
            GraphError::JsonError(_) => (ErrorCategory::Internal, "graph.json"),
            #[cfg(feature = "sqlite")]
            GraphError::DatabaseError(_) => (ErrorCategory::Internal, "graph.database"),
        };
        adk_core::AdkError::new(ErrorComponent::Graph, category, code, err.to_string())
            .with_source(err)
    }
}
