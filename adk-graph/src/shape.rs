//! The static shape of a compiled graph, for workflow observers (spec 3.3 §12, developer
//! track).
//!
//! [`CompiledGraph::shape`] reports every node, every edge and the entry nodes in a
//! deterministic order, so an observer draws the same graph on every call and a
//! governed node's [`ToolOrigin`](adk_core::ToolOrigin) binds to a stable declaration.
//!
//! # Example
//!
//! ```
//! use adk_graph::edge::{END, START};
//! use adk_graph::graph::StateGraph;
//! use adk_graph::node::NodeOutput;
//! use adk_graph::shape::{EdgeShape, NodeKind};
//!
//! let graph = StateGraph::with_channels(&["invoice"])
//!     .add_node_fn("draft", |_ctx| async { Ok(NodeOutput::new()) })
//!     .add_edge(START, "draft")
//!     .add_edge("draft", END)
//!     .compile()?;
//!
//! let shape = graph.shape();
//! assert_eq!(shape.entry, vec!["draft".to_string()]);
//! assert_eq!(shape.nodes[0].kind, NodeKind::Function);
//! assert_eq!(shape.edges, vec![EdgeShape::new("draft", END, false)]);
//! # Ok::<(), adk_graph::GraphError>(())
//! ```

use crate::edge::{END, Edge, EdgeTarget, START};
use crate::governed::GovernedNode;
use crate::graph::CompiledGraph;
use adk_core::ToolEffect;
use serde::{Deserialize, Serialize};

/// What kind of step a node is, as a workflow observer labels it (spec 3.3 §12).
///
/// A node reports its own kind through [`Node::kind`](crate::node::Node::kind); a fan-in
/// node is reported as [`NodeKind::Join`] whatever it runs.
///
/// # Example
///
/// ```
/// use adk_graph::shape::NodeKind;
///
/// assert_eq!(serde_json::to_value(NodeKind::HttpCall)?, serde_json::json!("httpCall"));
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum NodeKind {
    /// An [`AgentNode`](crate::node::AgentNode): a model step.
    ModelStep,
    /// A function node.
    Function,
    /// An HTTP action node.
    HttpCall,
    /// A trigger action node.
    Trigger,
    /// A set action node.
    Set,
    /// A transform action node.
    Transform,
    /// A switch action node.
    Switch,
    /// A loop action node.
    Loop,
    /// A merge action node.
    Merge,
    /// A wait action node.
    Wait,
    /// A file action node.
    File,
    /// A code action node.
    Code,
    /// A database action node.
    Database,
    /// An email action node.
    Email,
    /// A notification action node.
    Notification,
    /// An RSS action node.
    Rss,
    /// A fan-in node, which waits for its predecessors.
    Join,
    /// A [`SubgraphNode`](crate::subgraph::SubgraphNode).
    Subgraph,
    /// Any other node.
    Other,
}

/// One node of a [`GraphShape`] (spec 3.3 §12).
///
/// # Example
///
/// ```
/// use adk_graph::governed::GovernedNode;
/// use adk_graph::shape::{NodeKind, NodeShape};
///
/// let shape = NodeShape {
///     name: "pay".to_string(),
///     description: "Charges the customer".to_string(),
///     kind: NodeKind::HttpCall,
///     effect: Some(adk_core::ToolEffect::NonIdempotent),
///     interrupt_before: false,
///     interrupt_after: false,
///     governed: Some(GovernedNode::new().with_approval().spending_money()),
/// };
/// let json = serde_json::to_value(&shape)?;
/// assert_eq!(json["governed"]["spendsMoney"], true);
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeShape {
    /// The node's name.
    pub name: String,
    /// The node's [`description`](crate::node::Node::description).
    pub description: String,
    /// What kind of step the node is.
    pub kind: NodeKind,
    /// The side effect the node declared with
    /// [`CompiledGraph::with_node_effect`], if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<ToolEffect>,
    /// Whether the run pauses before the node.
    pub interrupt_before: bool,
    /// Whether the run pauses after the node.
    pub interrupt_after: bool,
    /// The node's governance, when it opted in with
    /// [`CompiledGraph::with_governed_node`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub governed: Option<GovernedNode>,
}

/// One edge of a [`GraphShape`]; `to` is [`END`] for an edge that ends a branch.
///
/// # Example
///
/// ```
/// use adk_graph::shape::EdgeShape;
///
/// let edge = EdgeShape::new("pay", "receipt", false);
/// assert_eq!(serde_json::to_value(&edge)?["to"], "receipt");
/// # Ok::<(), serde_json::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeShape {
    /// The node the edge leaves.
    pub from: String,
    /// The node the edge reaches, or [`END`].
    pub to: String,
    /// Whether a router chooses this edge at run time.
    pub conditional: bool,
}

impl EdgeShape {
    /// An edge from `from` to `to`.
    pub fn new(from: impl Into<String>, to: impl Into<String>, conditional: bool) -> Self {
        Self { from: from.into(), to: to.into(), conditional }
    }
}

/// The static shape of a compiled graph (spec 3.3 §12).
///
/// Nodes are sorted by name, edges by `(from, to, conditional)` and entry nodes by name,
/// so equal graphs serialize identically.
///
/// # Example
///
/// ```
/// use adk_graph::edge::{END, START};
/// use adk_graph::graph::StateGraph;
/// use adk_graph::node::NodeOutput;
///
/// let graph = StateGraph::with_channels(&["n"])
///     .add_node_fn("a", |_ctx| async { Ok(NodeOutput::new()) })
///     .add_edge(START, "a")
///     .add_edge("a", END)
///     .compile()?;
/// let json = serde_json::to_value(graph.shape())?;
/// assert_eq!(json["entry"], serde_json::json!(["a"]));
/// assert_eq!(json["nodes"][0]["interruptBefore"], false);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphShape {
    /// Every node, sorted by name.
    pub nodes: Vec<NodeShape>,
    /// Every edge, sorted, with each target of a conditional edge listed once.
    pub edges: Vec<EdgeShape>,
    /// The nodes a run starts with, sorted.
    pub entry: Vec<String>,
}

impl CompiledGraph {
    /// The graph's nodes, edges and entry nodes, for a workflow observer (spec 3.3 §12).
    ///
    /// # Example
    ///
    /// ```
    /// use adk_graph::edge::{END, START};
    /// use adk_graph::graph::StateGraph;
    /// use adk_graph::node::NodeOutput;
    /// use adk_graph::shape::EdgeShape;
    ///
    /// let graph = StateGraph::with_channels(&["route"])
    ///     .add_node_fn("triage", |_ctx| async { Ok(NodeOutput::new()) })
    ///     .add_node_fn("refund", |_ctx| async { Ok(NodeOutput::new()) })
    ///     .add_edge(START, "triage")
    ///     .add_conditional_edges(
    ///         "triage",
    ///         |_state| "refund".to_string(),
    ///         [("refund", "refund"), (END, END)],
    ///     )
    ///     .add_edge("refund", END)
    ///     .compile()?;
    /// assert_eq!(
    ///     graph.shape().edges,
    ///     vec![
    ///         EdgeShape::new("refund", END, false),
    ///         EdgeShape::new("triage", END, true),
    ///         EdgeShape::new("triage", "refund", true),
    ///     ]
    /// );
    /// # Ok::<(), adk_graph::GraphError>(())
    /// ```
    pub fn shape(&self) -> GraphShape {
        let mut names: Vec<&String> = self.nodes.keys().collect();
        names.sort();
        let nodes = names.into_iter().filter_map(|name| self.node_shape(name)).collect();

        let mut edges: Vec<EdgeShape> = Vec::new();
        let target_name = |target: &EdgeTarget| match target {
            EdgeTarget::Node(name) => name.clone(),
            EdgeTarget::End => END.to_string(),
        };
        for edge in &self.edges {
            match edge {
                Edge::Direct { source, target } if source != START => {
                    edges.push(EdgeShape::new(source.clone(), target_name(target), false));
                }
                Edge::Conditional { source, targets, .. } => {
                    edges.extend(
                        targets.values().map(|target| {
                            EdgeShape::new(source.clone(), target_name(target), true)
                        }),
                    );
                }
                Edge::Direct { .. } | Edge::Entry { .. } => {}
            }
        }
        edges.sort();
        edges.dedup();

        let mut entry = self.get_entry_nodes();
        entry.sort();
        entry.dedup();
        GraphShape { nodes, edges, entry }
    }

    /// The shape entry of one node, or `None` when the graph has no such node.
    pub(crate) fn node_shape(&self, name: &str) -> Option<NodeShape> {
        let node = self.nodes.get(name)?;
        let kind =
            if self.deferred_configs.contains_key(name) { NodeKind::Join } else { node.kind() };
        Some(NodeShape {
            name: name.to_string(),
            description: node.description().to_string(),
            kind,
            effect: self.node_effects.get(name).copied(),
            interrupt_before: self.interrupt_before.contains(name),
            interrupt_after: self.interrupt_after.contains(name),
            governed: self.governed_nodes.get(name).copied(),
        })
    }
}
