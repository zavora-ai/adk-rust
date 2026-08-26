//! Human-in-the-loop interrupt types

use adk_core::ToolConfirmationRequest;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Interrupt request from a node or configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Interrupt {
    /// Interrupt before executing a node
    Before(String),
    /// Interrupt after executing a node
    After(String),
    /// Dynamic interrupt from within a node
    Dynamic {
        /// Message to display to the user
        message: String,
        /// Optional data for the interrupt
        data: Option<Value>,
    },
}

impl std::fmt::Display for Interrupt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Before(node) => write!(f, "Interrupt before '{}'", node),
            Self::After(node) => write!(f, "Interrupt after '{}'", node),
            Self::Dynamic { message, .. } => write!(f, "Dynamic interrupt: {}", message),
        }
    }
}

/// Helper to create a dynamic interrupt from within a node
pub fn interrupt(message: &str) -> Interrupt {
    Interrupt::Dynamic { message: message.to_string(), data: None }
}

/// Helper to create a dynamic interrupt with data
pub fn interrupt_with_data(message: &str, data: Value) -> Interrupt {
    Interrupt::Dynamic { message: message.to_string(), data: Some(data) }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingGraphToolConfirmation {
    node: String,
    request: ToolConfirmationRequest,
}

/// A persisted tool-authorization pause emitted by a graph.
///
/// Use [`Self::from_stream_event`] when directly streaming a graph, or
/// [`GraphInterruptPayload::tool_confirmation`] when the graph runs as a
/// [`crate::agent::GraphAgent`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphToolConfirmationPause {
    /// Graph node whose agent requested authorization.
    pub node: String,
    /// The exact tool call the caller must approve or deny.
    pub request: ToolConfirmationRequest,
    /// Thread to resume after a decision.
    pub thread_id: String,
    /// Checkpoint saved before the pause was reported.
    pub checkpoint_id: String,
}

impl GraphToolConfirmationPause {
    /// Reserved custom-event and dynamic-interrupt marker for tool confirmation.
    pub const KIND: &str = "tool_confirmation";

    /// Read a tool-confirmation pause from a graph stream event.
    pub fn from_stream_event(event: &crate::stream::StreamEvent) -> Option<Self> {
        let crate::stream::StreamEvent::Custom { event_type, data, .. } = event else {
            return None;
        };
        (event_type == Self::KIND).then(|| serde_json::from_value(data.clone()).ok()).flatten()
    }

    /// Read a tool-confirmation pause from an interrupted graph execution.
    pub fn from_interrupted_execution(
        interrupted: &crate::error::InterruptedExecution,
    ) -> Option<Self> {
        let (node, request) = pending_from_interrupt(&interrupted.interrupt)?;
        Some(Self {
            node,
            request,
            thread_id: interrupted.thread_id.clone(),
            checkpoint_id: interrupted.checkpoint_id.clone(),
        })
    }

    pub(crate) fn pending_interrupt(node: String, request: ToolConfirmationRequest) -> Interrupt {
        Interrupt::Dynamic {
            message: Self::KIND.to_string(),
            data: Some(Self::pending_data(node, request)),
        }
    }

    pub(crate) fn pending_data(node: String, request: ToolConfirmationRequest) -> Value {
        serde_json::to_value(PendingGraphToolConfirmation { node, request }).unwrap_or(Value::Null)
    }
}

fn pending_from_interrupt(interrupt: &Interrupt) -> Option<(String, ToolConfirmationRequest)> {
    let Interrupt::Dynamic { message, data } = interrupt else { return None };
    if message != GraphToolConfirmationPause::KIND {
        return None;
    }
    let pending: PendingGraphToolConfirmation = serde_json::from_value(data.clone()?).ok()?;
    Some((pending.node, pending.request))
}

/// The reserved `Event::provider_metadata` key carrying a graph interrupt.
pub const INTERRUPT_METADATA_KEY: &str = "adk.graph.interrupt";

/// A graph interrupt as it crosses the [`Agent`](adk_core::Agent) boundary.
///
/// `GraphAgent` cannot return `GraphError::Interrupted` to a `Runner`: the trait
/// yields events, and an error would end the invocation. It therefore emits one
/// event carrying this payload, so a caller can read which node paused, why, and
/// which checkpoint to resume from.
///
/// It travels as JSON in `Event::provider_metadata` under
/// [`INTERRUPT_METADATA_KEY`] rather than as a field on `adk_core::EventActions`.
/// A graph-shaped type in `adk-core` would put a Tier 3 concept in a Tier 1
/// crate; every other consumer of the event is unaffected by an extra metadata
/// key.
///
/// # Example
///
/// ```rust,no_run
/// use adk_graph::interrupt::GraphInterruptPayload;
/// # fn handle(event: &adk_core::Event) {
/// if let Some(pause) = GraphInterruptPayload::from_event(event) {
///     println!("paused at {:?}: {:?}", pause.node, pause.message);
///     // Resume by invoking the same thread again, supplying any decision.
/// }
/// # }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphInterruptPayload {
    /// `"before"`, `"after"`, or `"dynamic"`.
    pub kind: String,
    /// The gated node, for a static interrupt. `None` for a dynamic one, which
    /// carries a message instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// The message a node supplied, for a dynamic interrupt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The data a node attached with `NodeOutput::interrupt_with_data`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    /// The thread to resume.
    pub thread_id: String,
    /// The checkpoint the run stopped at.
    pub checkpoint_id: String,
}

impl GraphInterruptPayload {
    /// Build a payload from an interrupt and the run it stopped.
    pub fn new(interrupt: &Interrupt, thread_id: &str, checkpoint_id: &str) -> Self {
        let (kind, node, message, data) = match interrupt {
            Interrupt::Before(node) => ("before", Some(node.clone()), None, None),
            Interrupt::After(node) => ("after", Some(node.clone()), None, None),
            Interrupt::Dynamic { message, data } => {
                ("dynamic", None, Some(message.clone()), data.clone())
            }
        };
        Self {
            kind: kind.to_string(),
            node,
            message,
            data,
            thread_id: thread_id.to_string(),
            checkpoint_id: checkpoint_id.to_string(),
        }
    }

    /// Read the payload from an event, or `None` if the event is not a graph
    /// interrupt.
    pub fn from_event(event: &adk_core::Event) -> Option<Self> {
        let raw = event.provider_metadata.get(INTERRUPT_METADATA_KEY)?;
        serde_json::from_str(raw).ok()
    }

    /// Build a graph-agent interrupt payload for a tool-confirmation pause.
    pub fn from_tool_confirmation_pause(pause: GraphToolConfirmationPause) -> Self {
        let thread_id = pause.thread_id.clone();
        let checkpoint_id = pause.checkpoint_id.clone();
        Self {
            kind: GraphToolConfirmationPause::KIND.to_string(),
            node: Some(pause.node.clone()),
            message: None,
            data: serde_json::to_value(pause).ok(),
            thread_id,
            checkpoint_id,
        }
    }

    /// Read the structured tool-confirmation pause, if this payload represents one.
    pub fn tool_confirmation(&self) -> Option<GraphToolConfirmationPause> {
        if self.kind != GraphToolConfirmationPause::KIND {
            return None;
        }
        serde_json::from_value(self.data.clone()?).ok()
    }

    /// Serialize for transport in `Event::provider_metadata`.
    pub fn to_metadata_value(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}
