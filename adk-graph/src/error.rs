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

    /// ADK core error
    #[error("ADK error: {0}")]
    AdkError(#[from] adk_core::AdkError),

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
