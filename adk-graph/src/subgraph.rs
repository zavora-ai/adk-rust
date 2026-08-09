//! Running one graph as a node of another.
//!
//! A subgraph keeps its own channels, its own edges and its own interrupt gates,
//! and exchanges named channels with its parent. Nesting through
//! [`AgentNode`](crate::node::AgentNode) instead would force the state through the
//! `Agent` boundary as `Content`, and a pause inside would arrive as an event the
//! parent reports rather than a pause the parent honours.
//!
//! # Channels are checked when the parent compiles
//!
//! Both schemas are known before anything runs, so a mapping that names a channel
//! neither side declares is a [`compile`](crate::graph::StateGraph::compile) error
//! naming the channel and the side. A mismatch cannot reach a run and surface as
//! an absent value.
//!
//! # Example
//!
//! ```
//! use adk_graph::edge::{END, START};
//! use adk_graph::graph::StateGraph;
//! use adk_graph::node::NodeOutput;
//! use adk_graph::subgraph::SubgraphNode;
//! use serde_json::json;
//! use std::sync::Arc;
//!
//! // The inner graph knows nothing about its parent.
//! let inner = StateGraph::with_channels(&["text", "length"])
//!     .add_node_fn("measure", |ctx| async move {
//!         let text = ctx.get("text").and_then(|v| v.as_str()).unwrap_or("");
//!         Ok(NodeOutput::new().with_update("length", json!(text.len())))
//!     })
//!     .add_edge(START, "measure")
//!     .add_edge("measure", END)
//!     .compile()?;
//!
//! let outer = StateGraph::with_channels(&["document", "size"])
//!     .add_node(
//!         SubgraphNode::new("measure_doc", Arc::new(inner))
//!             .with_input("document", "text")
//!             .with_output("length", "size"),
//!     )
//!     .add_edge(START, "measure_doc")
//!     .add_edge("measure_doc", END)
//!     .compile()?;
//! # let _ = outer;
//! # Ok::<(), adk_graph::error::GraphError>(())
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::error::{GraphError, Result};
use crate::graph::CompiledGraph;
use crate::interrupt::Interrupt;
use crate::node::{ExecutionConfig, Node, NodeContext, NodeOutput};
use crate::state::{State, StateSchema};

/// Which side of a subgraph mapping a channel name belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelSide {
    /// A channel of the graph that holds the subgraph.
    Parent,
    /// A channel of the subgraph itself.
    Child,
}

impl std::fmt::Display for ChannelSide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parent => write!(f, "parent"),
            Self::Child => write!(f, "subgraph"),
        }
    }
}

/// One graph running as a node of another.
///
/// See the [module documentation](self) for the channel rules.
pub struct SubgraphNode {
    name: String,
    graph: Arc<CompiledGraph>,
    /// Parent channel to subgraph channel, applied before the subgraph runs.
    inputs: Vec<(String, String)>,
    /// Subgraph channel to parent channel, applied after it finishes.
    outputs: Vec<(String, String)>,
    /// Whether channels the two schemas share pass through without being named.
    share_by_name: bool,
}

impl SubgraphNode {
    /// Wraps a compiled graph as a node.
    ///
    /// Channels the two schemas declare under the same name pass through in both
    /// directions. Add [`Self::with_input`] or [`Self::with_output`] for channels
    /// whose names differ, or call [`Self::isolated`] to pass nothing implicitly.
    pub fn new(name: impl Into<String>, graph: Arc<CompiledGraph>) -> Self {
        Self {
            name: name.into(),
            graph,
            inputs: Vec::new(),
            outputs: Vec::new(),
            share_by_name: true,
        }
    }

    /// Feeds a parent channel into a subgraph channel under a different name.
    pub fn with_input(mut self, parent: impl Into<String>, child: impl Into<String>) -> Self {
        self.inputs.push((parent.into(), child.into()));
        self
    }

    /// Writes a subgraph channel back to a parent channel under a different name.
    pub fn with_output(mut self, child: impl Into<String>, parent: impl Into<String>) -> Self {
        self.outputs.push((child.into(), parent.into()));
        self
    }

    /// Exchanges only the channels named by `with_input` and `with_output`.
    ///
    /// Without this, a channel both schemas declare under the same name passes
    /// through. Isolating is worth the extra naming when the two graphs are
    /// maintained apart, because then adding a channel to one cannot silently
    /// start feeding the other.
    pub fn isolated(mut self) -> Self {
        self.share_by_name = false;
        self
    }

    /// The graph this node runs.
    pub fn graph(&self) -> &Arc<CompiledGraph> {
        &self.graph
    }

    /// Channels shared by name, when that is enabled.
    fn shared_with(&self, parent: &StateSchema) -> Vec<String> {
        if !self.share_by_name {
            return Vec::new();
        }
        let mut names: Vec<String> = self
            .graph
            .schema
            .channels
            .keys()
            .filter(|name| parent.channels.contains_key(*name))
            .cloned()
            .collect();
        names.sort();
        names
    }

    /// Builds the subgraph's input state from the parent's.
    fn project_in(&self, parent_state: &State, parent_schema: &StateSchema) -> State {
        let mut input = State::new();
        for name in self.shared_with(parent_schema) {
            if let Some(value) = parent_state.get(&name) {
                input.insert(name, value.clone());
            }
        }
        for (parent_name, child_name) in &self.inputs {
            if let Some(value) = parent_state.get(parent_name) {
                input.insert(child_name.clone(), value.clone());
            }
        }
        input
    }

    /// Builds the parent's updates from the subgraph's final state.
    fn project_out(
        &self,
        child_state: &State,
        parent_schema: &StateSchema,
    ) -> HashMap<String, Value> {
        let mut updates = HashMap::new();
        for name in self.shared_with(parent_schema) {
            if let Some(value) = child_state.get(&name) {
                updates.insert(name, value.clone());
            }
        }
        for (child_name, parent_name) in &self.outputs {
            if let Some(value) = child_state.get(child_name) {
                updates.insert(parent_name.clone(), value.clone());
            }
        }
        updates
    }

    /// The thread the subgraph runs on, derived from the parent's.
    ///
    /// Namespacing by node name keeps two subgraphs of one parent apart, and keeps
    /// a subgraph's checkpoints from colliding with its parent's.
    fn child_thread(&self, parent_thread: &str) -> String {
        format!("{parent_thread}/{}", self.name)
    }
}

#[async_trait]
impl Node for SubgraphNode {
    fn name(&self) -> &str {
        &self.name
    }

    /// Rejects a mapping that names a channel the relevant side does not declare.
    ///
    /// This runs when the parent compiles, so a mismatch never reaches a run.
    fn validate_against(&self, parent: &StateSchema) -> Result<()> {
        let child = &self.graph.schema;
        let mismatch = |channel: &str, side: ChannelSide| {
            Err(GraphError::SubgraphChannelMismatch {
                subgraph: self.name.clone(),
                channel: channel.to_string(),
                side: side.to_string(),
            })
        };

        for (parent_name, child_name) in &self.inputs {
            if !parent.channels.contains_key(parent_name) {
                return mismatch(parent_name, ChannelSide::Parent);
            }
            if !child.channels.contains_key(child_name) {
                return mismatch(child_name, ChannelSide::Child);
            }
        }
        for (child_name, parent_name) in &self.outputs {
            if !child.channels.contains_key(child_name) {
                return mismatch(child_name, ChannelSide::Child);
            }
            if !parent.channels.contains_key(parent_name) {
                return mismatch(parent_name, ChannelSide::Parent);
            }
        }

        // A subgraph that can pause but keeps no checkpoints cannot resume: the
        // parent re-enters it and it starts from its first node, repeating whatever
        // it had already done. Both facts are known now, so this is a compile
        // error rather than work silently paid for twice.
        if self.graph.can_pause() && !self.graph.has_checkpointer() {
            return Err(GraphError::InvalidGraph(format!(
                "subgraph '{}' has interrupt gates but no checkpointer, so a pause \
                 inside it could not be resumed and its finished work would run \
                 again. Add one with with_checkpointer",
                self.name
            )));
        }

        // A subgraph that exchanges nothing cannot affect its parent, which is
        // almost always a naming mistake rather than an intention.
        if self.inputs.is_empty() && self.outputs.is_empty() && self.shared_with(parent).is_empty()
        {
            return Err(GraphError::InvalidGraph(format!(
                "subgraph '{}' exchanges no channels with its parent. Name them with \
                 with_input and with_output, or share a channel name",
                self.name
            )));
        }
        Ok(())
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput> {
        let parent_schema = ctx.parent_schema().ok_or_else(|| {
            GraphError::InvalidGraph(format!(
                "subgraph '{}' ran without its parent's schema. This is an executor \
                 defect, not a configuration error",
                self.name
            ))
        })?;

        let input = self.project_in(&ctx.state, &parent_schema);
        let thread = self.child_thread(&ctx.config.thread_id);
        let config = ExecutionConfig::new(&thread);

        match self.graph.invoke_detailed(input, config).await {
            Ok(outcome) => {
                let mut output = NodeOutput::new()
                    .with_updates(self.project_out(&outcome.state, &parent_schema));
                // A node inside asked for a node of this graph's parent. Becoming
                // this node's own goto is what makes it happen, and it also means
                // the parent validates the target, as it does for any goto.
                if let Some(targets) = outcome.goto_parent {
                    output = output.with_goto(targets);
                }
                Ok(output)
            }
            // A pause inside is a pause of the whole run. Reported with the
            // subgraph's name in front of the inner node, so a deep pause says
            // where it happened, and the subgraph's own thread holds the state to
            // resume from.
            Err(GraphError::Interrupted(inner)) => {
                let message = match &inner.interrupt {
                    Interrupt::Dynamic { message, .. } => message.clone(),
                    other => other.to_string(),
                };
                let data = match &inner.interrupt {
                    Interrupt::Dynamic { data, .. } => data.clone(),
                    _ => None,
                };
                let mut payload = serde_json::Map::new();
                payload.insert("subgraph".to_string(), Value::String(self.name.clone()));
                payload.insert("thread".to_string(), Value::String(thread));
                if let Some(data) = data {
                    payload.insert("data".to_string(), data);
                }
                Ok(NodeOutput::interrupt_with_data(
                    &format!("{}: {message}", self.name),
                    Value::Object(payload),
                ))
            }
            Err(error) => Err(error),
        }
    }
}
