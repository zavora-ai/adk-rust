//! Error types for adk-graph

use std::time::Duration;

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

    /// Node timed out
    #[error("Node '{node}' timed out after {elapsed:?}")]
    NodeTimedOut { node: String, elapsed: Duration },

    /// Fan-in node timed out waiting for upstream paths
    #[error("Fan-in node '{node}' timed out: received {received}/{expected} upstream outputs")]
    FanInTimedOut { node: String, received: usize, expected: usize },

    /// State serialization error
    #[error("State serialization error: {0}")]
    SerializationError(String),

    /// Checkpoint error
    #[error("Checkpoint error: {0}")]
    CheckpointError(String),

    /// A node wrote a channel the state schema does not declare.
    ///
    /// Only raised when channel enforcement is on. An undeclared channel
    /// otherwise takes the overwrite reducer, which silently discards the
    /// appends a list channel was meant to collect.
    #[error(
        "node '{node}' wrote undeclared channel '{channel}'. Declare it on the graph, or drop the write"
    )]
    UndeclaredChannel {
        /// The node that produced the update.
        node: String,
        /// The channel name that is not declared.
        channel: String,
    },

    /// A subgraph mapping names a channel the relevant side does not declare.
    #[error("subgraph '{subgraph}' maps channel '{channel}', which the {side} does not declare")]
    SubgraphChannelMismatch {
        /// The subgraph node's name.
        subgraph: String,
        /// The channel that is not declared.
        channel: String,
        /// Which side is missing it, the parent or the subgraph.
        side: String,
    },

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

    /// Other error (used by extensions like the functional API)
    #[error("{0}")]
    Other(String),
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
            GraphError::NodeTimedOut { .. } => (ErrorCategory::Timeout, "graph.node_timed_out"),
            GraphError::FanInTimedOut { .. } => (ErrorCategory::Timeout, "graph.fan_in_timed_out"),
            GraphError::SerializationError(_) => (ErrorCategory::Internal, "graph.serialization"),
            GraphError::CheckpointError(_) => (ErrorCategory::Internal, "graph.checkpoint"),
            GraphError::SubgraphChannelMismatch { .. } => {
                (ErrorCategory::InvalidInput, "graph.subgraph_channel_mismatch")
            }
            GraphError::UndeclaredChannel { .. } => {
                (ErrorCategory::InvalidInput, "graph.undeclared_channel")
            }
            GraphError::UnknownRouteTarget(_) => {
                (ErrorCategory::NotFound, "graph.unknown_route_target")
            }
            GraphError::IoError(_) => (ErrorCategory::Internal, "graph.io"),
            GraphError::JsonError(_) => (ErrorCategory::Internal, "graph.json"),
            #[cfg(feature = "sqlite")]
            GraphError::DatabaseError(_) => (ErrorCategory::Internal, "graph.database"),
            GraphError::Other(_) => (ErrorCategory::Internal, "graph.other"),
        };
        adk_core::AdkError::new(ErrorComponent::Graph, category, code, err.to_string())
            .with_source(err)
    }
}
