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
        /// The pause's id, set by the executor when the node pauses: stable for one
        /// pause and bound to its thread, step, node and question ([`interrupt_id`]).
        /// Answer it with
        /// [`ExecutionConfig::with_resume_value`](crate::node::ExecutionConfig::with_resume_value)
        /// under this id.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// JSON Schema the answer must satisfy; an answer that does not is refused
        /// when the run resumes, before any node runs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response_schema: Option<Value>,
    },
}

impl Interrupt {
    /// The pause's id, for a dynamic interrupt the executor reported.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_graph::interrupt::interrupt;
    ///
    /// assert_eq!(interrupt("approve?").id(), None, "the executor assigns it");
    /// ```
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Dynamic { id, .. } => id.as_deref(),
            Self::Before(_) | Self::After(_) => None,
        }
    }

    /// The JSON Schema the answer must satisfy, when the interrupt declares one.
    pub fn response_schema(&self) -> Option<&Value> {
        match self {
            Self::Dynamic { response_schema, .. } => response_schema.as_ref(),
            Self::Before(_) | Self::After(_) => None,
        }
    }

    /// Declares the JSON Schema the answer to this dynamic interrupt must satisfy; a
    /// static gate is returned unchanged.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_graph::interrupt::interrupt;
    /// use serde_json::json;
    ///
    /// let pause = interrupt("approve the refund?").with_response_schema(json!({
    ///     "type": "object",
    ///     "properties": { "approved": { "type": "boolean" } },
    ///     "required": ["approved"]
    /// }));
    /// assert!(pause.response_schema().is_some());
    /// ```
    #[must_use]
    pub fn with_response_schema(mut self, schema: Value) -> Self {
        if let Self::Dynamic { response_schema, .. } = &mut self {
            *response_schema = Some(schema);
        }
        self
    }

    /// Gives a dynamic interrupt raised by `node` at `step` of `thread_id` its id, unless
    /// it has one.
    pub(crate) fn identify(&mut self, thread_id: &str, step: usize, node: &str) {
        if let Self::Dynamic { message, data, id, response_schema } = self
            && id.is_none()
        {
            *id = Some(interrupt_id(
                thread_id,
                step,
                node,
                message,
                data.as_ref(),
                response_schema.as_ref(),
            ));
        }
    }
}

/// The id of the pause `node` raised at `step` of `thread_id` with `message`, `data` and
/// `response_schema`: `int_` and 32 hex characters of a SHA-256 over all of them.
///
/// The same pause, re-raised because the run resumed without an answer, gets the same id;
/// a later pause — the node's next pass through a loop, or the same step asking about
/// other data — gets another, so an answer given to one pause never answers another. This
/// is how an approval binds to its call's fingerprint.
///
/// # Example
///
/// ```
/// use adk_graph::interrupt::interrupt_id;
/// use serde_json::json;
///
/// let first = interrupt_id("t1", 3, "review", "approve?", Some(&json!({ "amount": 40 })), None);
/// assert!(first.starts_with("int_"));
/// assert_eq!(first, interrupt_id("t1", 3, "review", "approve?", Some(&json!({ "amount": 40 })), None));
/// assert_ne!(first, interrupt_id("t1", 3, "review", "approve?", Some(&json!({ "amount": 900 })), None));
/// assert_ne!(first, interrupt_id("t1", 7, "review", "approve?", Some(&json!({ "amount": 40 })), None));
/// ```
pub fn interrupt_id(
    thread_id: &str,
    step: usize,
    node: &str,
    message: &str,
    data: Option<&Value>,
    response_schema: Option<&Value>,
) -> String {
    use sha2::{Digest, Sha256};
    let canonical = serde_json::json!([thread_id, step, node, message, data, response_schema]);
    let digest = Sha256::digest(canonical.to_string().as_bytes());
    let hex: String = digest.iter().take(16).map(|byte| format!("{byte:02x}")).collect();
    format!("int_{hex}")
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
    Interrupt::Dynamic { message: message.to_string(), data: None, id: None, response_schema: None }
}

/// Helper to create a dynamic interrupt with data
pub fn interrupt_with_data(message: &str, data: Value) -> Interrupt {
    Interrupt::Dynamic {
        message: message.to_string(),
        data: Some(data),
        id: None,
        response_schema: None,
    }
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
    ///
    /// Reads the first pause; [`Self::all_from_interrupted_execution`] reads every
    /// pause of the step.
    pub fn from_interrupted_execution(
        interrupted: &crate::error::InterruptedExecution,
    ) -> Option<Self> {
        Self::from_interrupt(
            &interrupted.interrupt,
            &interrupted.thread_id,
            &interrupted.checkpoint_id,
        )
    }

    /// Read every tool-confirmation pause of an interrupted graph execution, in
    /// node-name order.
    pub fn all_from_interrupted_execution(
        interrupted: &crate::error::InterruptedExecution,
    ) -> Vec<Self> {
        interrupted
            .interrupts
            .iter()
            .filter_map(|interrupt| {
                Self::from_interrupt(interrupt, &interrupted.thread_id, &interrupted.checkpoint_id)
            })
            .collect()
    }

    /// Read a tool-confirmation pause from one interrupt of the run that stopped
    /// at `checkpoint_id` on `thread_id`.
    pub fn from_interrupt(
        interrupt: &Interrupt,
        thread_id: &str,
        checkpoint_id: &str,
    ) -> Option<Self> {
        let (node, request) = pending_from_interrupt(interrupt)?;
        Some(Self {
            node,
            request,
            thread_id: thread_id.to_string(),
            checkpoint_id: checkpoint_id.to_string(),
        })
    }

    pub(crate) fn pending_interrupt(node: String, request: ToolConfirmationRequest) -> Interrupt {
        Interrupt::Dynamic {
            message: Self::KIND.to_string(),
            data: Some(Self::pending_data(node, request)),
            id: None,
            response_schema: None,
        }
    }

    pub(crate) fn pending_data(node: String, request: ToolConfirmationRequest) -> Value {
        serde_json::to_value(PendingGraphToolConfirmation { node, request }).unwrap_or(Value::Null)
    }
}

fn pending_from_interrupt(interrupt: &Interrupt) -> Option<(String, ToolConfirmationRequest)> {
    let Interrupt::Dynamic { message, data, .. } = interrupt else { return None };
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
    /// The pause's id, for a dynamic interrupt: answer it under this key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupt_id: Option<String>,
    /// The JSON Schema the answer must satisfy, when the node declared one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<Value>,
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
            Interrupt::Dynamic { message, data, .. } => {
                ("dynamic", None, Some(message.clone()), data.clone())
            }
        };
        Self {
            kind: kind.to_string(),
            node,
            message,
            data,
            interrupt_id: interrupt.id().map(str::to_string),
            response_schema: interrupt.response_schema().cloned(),
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
            interrupt_id: None,
            response_schema: None,
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
