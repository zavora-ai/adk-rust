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

    /// The agent an [`AgentNode`](crate::node::AgentNode) runs failed.
    ///
    /// Carries the agent's own error, so its category, code, retry hint, and
    /// upstream status reach the caller: a rate-limited model is still
    /// `RateLimited` once the graph reports it.
    #[error("node '{node}' agent failed: {source}")]
    AgentFailed {
        /// The node whose agent failed.
        node: String,
        /// The error the agent returned or yielded.
        #[source]
        source: Box<adk_core::AdkError>,
    },

    /// Node timed out
    #[error("Node '{node}' timed out after {elapsed:?}")]
    NodeTimedOut { node: String, elapsed: Duration },

    /// Fan-in node timed out waiting for upstream paths
    #[error("Fan-in node '{node}' timed out: received {received}/{expected} upstream outputs")]
    FanInTimedOut { node: String, received: usize, expected: usize },

    /// A fan-in node's remaining predecessors can no longer run, and fewer than
    /// its `min_predecessors` arrived.
    #[error(
        "fan-in node '{node}' needs {required} upstream outputs but received {received}, and no \
         other predecessor can still run in this pass. Lower `min_predecessors`, or route every \
         required branch to the node"
    )]
    FanInIncomplete {
        /// The fan-in node.
        node: String,
        /// How many predecessors arrived.
        received: usize,
        /// The `min_predecessors` it was configured with.
        required: usize,
    },

    /// State serialization error
    #[error("State serialization error: {0}")]
    SerializationError(String),

    /// Checkpoint error, including a failure of the checkpointer's database.
    #[error("Checkpoint error: {0}")]
    CheckpointError(String),

    /// A checkpoint was refused because its thread already holds a checkpoint with an
    /// equal or higher version.
    ///
    /// Raised when two writers continue one thread: the writer that falls behind is
    /// refused instead of interleaving its checkpoints with the other's.
    #[error(
        "checkpoint version {version} for thread '{thread_id}' conflicts with stored version {head}. \
         Another writer advanced the thread; reload its newest checkpoint and continue from version {head} + 1"
    )]
    CheckpointConflict {
        /// The thread the checkpoint belongs to.
        thread_id: String,
        /// The version the writer tried to store.
        version: u64,
        /// The highest version already stored for the thread.
        head: u64,
    },

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

    /// A functional-API workflow suspended at an interrupt and needs an answer.
    ///
    /// Supply the answer under `continuation_key` with
    /// [`ExecutionConfig::with_resume_value`](crate::node::ExecutionConfig::with_resume_value)
    /// and invoke the entrypoint again. A `#[task]` retry policy does not retry it.
    #[error("workflow suspended at interrupt '{continuation_key}': {message}")]
    Suspended {
        /// Key identifying the interrupt site within the thread.
        continuation_key: String,
        /// The message passed to the interrupt.
        message: String,
    },

    /// The caller cancelled a functional-API run.
    ///
    /// Raised when the run's
    /// [`ExecutionConfig::cancellation_token`](crate::node::ExecutionConfig) is
    /// cancelled. A `#[task]` retry policy does not retry it.
    #[error("workflow cancelled")]
    Cancelled,

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
    /// The interrupt that occurred: the first of [`Self::interrupts`].
    pub interrupt: Interrupt,
    /// Every pause of the step, in node-name order.
    ///
    /// Nodes of one super-step run together, so several can pause at once, such
    /// as two agent nodes each asking to confirm a tool call. A static gate is a
    /// single pause.
    pub interrupts: Vec<Interrupt>,
    /// Current state at interruption
    pub state: crate::state::State,
    /// Step number when interrupted
    pub step: usize,
}

impl InterruptedExecution {
    /// Create a new interrupted execution with a single pause.
    pub fn new(
        thread_id: String,
        checkpoint_id: String,
        interrupt: Interrupt,
        state: crate::state::State,
        step: usize,
    ) -> Self {
        let interrupts = vec![interrupt.clone()];
        Self { thread_id, checkpoint_id, interrupt, interrupts, state, step }
    }

    /// Records every pause of the step; the first stays [`Self::interrupt`].
    ///
    /// An empty list leaves the single pause given to [`Self::new`].
    #[must_use]
    pub fn with_interrupts(mut self, interrupts: Vec<Interrupt>) -> Self {
        if let Some(first) = interrupts.first() {
            self.interrupt = first.clone();
            self.interrupts = interrupts;
        }
        self
    }
}

/// Converts a graph error into the structured ADK error.
///
/// A [`GraphError::AgentFailed`] becomes the agent's own error, with the node
/// named in its message, so its category, code, and retry hint are unchanged.
/// Every other variant maps to an `ErrorComponent::Graph` error that carries the
/// graph error as its source.
impl From<GraphError> for adk_core::AdkError {
    fn from(err: GraphError) -> Self {
        use adk_core::{ErrorCategory, ErrorComponent};
        if let GraphError::AgentFailed { node, source } = err {
            let mut error = *source;
            error.message = format!("graph node '{node}': {}", error.message);
            return error;
        }
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
            GraphError::AgentFailed { source, .. } => (source.category, source.code),
            GraphError::NodeTimedOut { .. } => (ErrorCategory::Timeout, "graph.node_timed_out"),
            GraphError::FanInTimedOut { .. } => (ErrorCategory::Timeout, "graph.fan_in_timed_out"),
            GraphError::FanInIncomplete { .. } => {
                (ErrorCategory::Internal, "graph.fan_in_incomplete")
            }
            GraphError::SerializationError(_) => (ErrorCategory::Internal, "graph.serialization"),
            GraphError::CheckpointError(_) => (ErrorCategory::Internal, "graph.checkpoint"),
            GraphError::CheckpointConflict { .. } => {
                (ErrorCategory::InvalidInput, "graph.checkpoint.conflict")
            }
            GraphError::SubgraphChannelMismatch { .. } => {
                (ErrorCategory::InvalidInput, "graph.subgraph_channel_mismatch")
            }
            GraphError::UndeclaredChannel { .. } => {
                (ErrorCategory::InvalidInput, "graph.undeclared_channel")
            }
            GraphError::UnknownRouteTarget(_) => {
                (ErrorCategory::NotFound, "graph.unknown_route_target")
            }
            GraphError::Suspended { .. } => (ErrorCategory::Cancelled, "graph.suspended"),
            GraphError::Cancelled => (ErrorCategory::Cancelled, "graph.cancelled"),
            GraphError::IoError(_) => (ErrorCategory::Internal, "graph.io"),
            GraphError::JsonError(_) => (ErrorCategory::Internal, "graph.json"),
            GraphError::Other(_) => (ErrorCategory::Internal, "graph.other"),
        };
        adk_core::AdkError::new(ErrorComponent::Graph, category, code, err.to_string())
            .with_source(err)
    }
}
