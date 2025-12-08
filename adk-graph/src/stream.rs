//! Streaming types for graph execution

use crate::state::State;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;

/// Stream mode options
#[derive(Clone, Copy, Debug, Default)]
pub enum StreamMode {
    /// Full state after each super-step
    #[default]
    Values,
    /// Only state changes
    Updates,
    /// LLM tokens and messages
    Messages,
    /// Custom events from nodes
    Custom,
    /// Debug information
    Debug,
}

/// Events emitted during streaming
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    /// State snapshot
    State { state: State, step: usize },

    /// State updates from a node
    Updates { node: String, updates: HashMap<String, Value> },

    /// Message/token from LLM
    Message { node: String, content: String, is_final: bool },

    /// Custom event from node
    Custom { node: String, event_type: String, data: Value },

    /// Debug event
    Debug { event_type: String, data: Value },

    /// Node started execution
    NodeStart { node: String, step: usize },

    /// Node completed execution
    NodeEnd { node: String, step: usize, duration_ms: u64 },

    /// Super-step completed
    StepComplete { step: usize, nodes_executed: Vec<String> },

    /// Execution was interrupted
    Interrupted { node: String, message: String },

    /// Graph execution completed
    Done { state: State, total_steps: usize },

    /// Error occurred
    Error { message: String, node: Option<String> },
}

impl StreamEvent {
    /// Create a state event
    pub fn state(state: State, step: usize) -> Self {
        Self::State { state, step }
    }

    /// Create an updates event
    pub fn updates(node: &str, updates: HashMap<String, Value>) -> Self {
        Self::Updates { node: node.to_string(), updates }
    }

    /// Create a message event
    pub fn message(node: &str, content: &str, is_final: bool) -> Self {
        Self::Message { node: node.to_string(), content: content.to_string(), is_final }
    }

    /// Create a custom event
    pub fn custom(node: &str, event_type: &str, data: Value) -> Self {
        Self::Custom { node: node.to_string(), event_type: event_type.to_string(), data }
    }

    /// Create a debug event
    pub fn debug(event_type: &str, data: Value) -> Self {
        Self::Debug { event_type: event_type.to_string(), data }
    }

    /// Create a node start event
    pub fn node_start(node: &str, step: usize) -> Self {
        Self::NodeStart { node: node.to_string(), step }
    }

    /// Create a node end event
    pub fn node_end(node: &str, step: usize, duration_ms: u64) -> Self {
        Self::NodeEnd { node: node.to_string(), step, duration_ms }
    }

    /// Create a step complete event
    pub fn step_complete(step: usize, nodes_executed: Vec<String>) -> Self {
        Self::StepComplete { step, nodes_executed }
    }

    /// Create an interrupted event
    pub fn interrupted(node: &str, message: &str) -> Self {
        Self::Interrupted { node: node.to_string(), message: message.to_string() }
    }

    /// Create a done event
    pub fn done(state: State, total_steps: usize) -> Self {
        Self::Done { state, total_steps }
    }

    /// Create an error event
    pub fn error(message: &str, node: Option<&str>) -> Self {
        Self::Error { message: message.to_string(), node: node.map(|s| s.to_string()) }
    }
}
