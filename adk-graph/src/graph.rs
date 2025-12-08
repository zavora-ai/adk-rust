//! StateGraph builder for constructing graphs

use crate::checkpoint::Checkpointer;
use crate::edge::{Edge, EdgeTarget, RouterFn, END, START};
use crate::error::{GraphError, Result};
use crate::node::{FunctionNode, Node, NodeContext, NodeOutput};
use crate::state::{State, StateSchema};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;

/// Builder for constructing graphs
pub struct StateGraph {
    /// State schema
    pub schema: StateSchema,
    /// Registered nodes
    pub nodes: HashMap<String, Arc<dyn Node>>,
    /// Registered edges
    pub edges: Vec<Edge>,
}

impl StateGraph {
    /// Create a new graph with the given state schema
    pub fn new(schema: StateSchema) -> Self {
        Self { schema, nodes: HashMap::new(), edges: vec![] }
    }

    /// Create with a simple schema (just channel names, all overwrite)
    pub fn with_channels(channels: &[&str]) -> Self {
        Self::new(StateSchema::simple(channels))
    }

    /// Add a node to the graph
    pub fn add_node<N: Node + 'static>(mut self, node: N) -> Self {
        self.nodes.insert(node.name().to_string(), Arc::new(node));
        self
    }

    /// Add a function as a node
    pub fn add_node_fn<F, Fut>(self, name: &str, func: F) -> Self
    where
        F: Fn(NodeContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<NodeOutput>> + Send + 'static,
    {
        self.add_node(FunctionNode::new(name, func))
    }

    /// Add a direct edge from source to target
    pub fn add_edge(mut self, source: &str, target: &str) -> Self {
        let target = EdgeTarget::from(target);

        if source == START {
            // Find existing entry or create new one
            let entry_idx = self.edges.iter().position(|e| matches!(e, Edge::Entry { .. }));

            match entry_idx {
                Some(idx) => {
                    if let Edge::Entry { targets } = &mut self.edges[idx] {
                        if let EdgeTarget::Node(node) = &target {
                            if !targets.contains(node) {
                                targets.push(node.clone());
                            }
                        }
                    }
                }
                None => {
                    if let EdgeTarget::Node(node) = target {
                        self.edges.push(Edge::Entry { targets: vec![node] });
                    }
                }
            }
        } else {
            self.edges.push(Edge::Direct { source: source.to_string(), target });
        }

        self
    }

    /// Add a conditional edge with a router function
    pub fn add_conditional_edges<F, I>(mut self, source: &str, router: F, targets: I) -> Self
    where
        F: Fn(&State) -> String + Send + Sync + 'static,
        I: IntoIterator<Item = (&'static str, &'static str)>,
    {
        let targets_map: HashMap<String, EdgeTarget> =
            targets.into_iter().map(|(k, v)| (k.to_string(), EdgeTarget::from(v))).collect();

        self.edges.push(Edge::Conditional {
            source: source.to_string(),
            router: Arc::new(router),
            targets: targets_map,
        });

        self
    }

    /// Add a conditional edge with an Arc router (for pre-built routers)
    pub fn add_conditional_edges_arc<I>(
        mut self,
        source: &str,
        router: RouterFn,
        targets: I,
    ) -> Self
    where
        I: IntoIterator<Item = (&'static str, &'static str)>,
    {
        let targets_map: HashMap<String, EdgeTarget> =
            targets.into_iter().map(|(k, v)| (k.to_string(), EdgeTarget::from(v))).collect();

        self.edges.push(Edge::Conditional {
            source: source.to_string(),
            router,
            targets: targets_map,
        });

        self
    }

    /// Compile the graph for execution
    pub fn compile(self) -> Result<CompiledGraph> {
        self.validate()?;

        Ok(CompiledGraph {
            schema: self.schema,
            nodes: self.nodes,
            edges: self.edges,
            checkpointer: None,
            interrupt_before: HashSet::new(),
            interrupt_after: HashSet::new(),
            recursion_limit: 50,
        })
    }

    /// Validate the graph structure
    fn validate(&self) -> Result<()> {
        // Check for entry point
        let has_entry = self.edges.iter().any(|e| matches!(e, Edge::Entry { .. }));
        if !has_entry {
            return Err(GraphError::NoEntryPoint);
        }

        // Check all node references exist
        for edge in &self.edges {
            match edge {
                Edge::Direct { source, target } => {
                    if source != START && !self.nodes.contains_key(source) {
                        return Err(GraphError::NodeNotFound(source.clone()));
                    }
                    if let EdgeTarget::Node(name) = target {
                        if !self.nodes.contains_key(name) {
                            return Err(GraphError::EdgeTargetNotFound(name.clone()));
                        }
                    }
                }
                Edge::Conditional { source, targets, .. } => {
                    if !self.nodes.contains_key(source) {
                        return Err(GraphError::NodeNotFound(source.clone()));
                    }
                    for target in targets.values() {
                        if let EdgeTarget::Node(name) = target {
                            if !self.nodes.contains_key(name) {
                                return Err(GraphError::EdgeTargetNotFound(name.clone()));
                            }
                        }
                    }
                }
                Edge::Entry { targets } => {
                    for target in targets {
                        if !self.nodes.contains_key(target) {
                            return Err(GraphError::EdgeTargetNotFound(target.clone()));
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

/// A compiled graph ready for execution
pub struct CompiledGraph {
    pub(crate) schema: StateSchema,
    pub(crate) nodes: HashMap<String, Arc<dyn Node>>,
    pub(crate) edges: Vec<Edge>,
    pub(crate) checkpointer: Option<Arc<dyn Checkpointer>>,
    pub(crate) interrupt_before: HashSet<String>,
    pub(crate) interrupt_after: HashSet<String>,
    pub(crate) recursion_limit: usize,
}

impl CompiledGraph {
    /// Configure checkpointing
    pub fn with_checkpointer<C: Checkpointer + 'static>(mut self, checkpointer: C) -> Self {
        self.checkpointer = Some(Arc::new(checkpointer));
        self
    }

    /// Configure checkpointing with Arc
    pub fn with_checkpointer_arc(mut self, checkpointer: Arc<dyn Checkpointer>) -> Self {
        self.checkpointer = Some(checkpointer);
        self
    }

    /// Configure interrupt before specific nodes
    pub fn with_interrupt_before(mut self, nodes: &[&str]) -> Self {
        self.interrupt_before = nodes.iter().map(|s| s.to_string()).collect();
        self
    }

    /// Configure interrupt after specific nodes
    pub fn with_interrupt_after(mut self, nodes: &[&str]) -> Self {
        self.interrupt_after = nodes.iter().map(|s| s.to_string()).collect();
        self
    }

    /// Set recursion limit for cycles
    pub fn with_recursion_limit(mut self, limit: usize) -> Self {
        self.recursion_limit = limit;
        self
    }

    /// Get entry nodes
    pub fn get_entry_nodes(&self) -> Vec<String> {
        for edge in &self.edges {
            if let Edge::Entry { targets } = edge {
                return targets.clone();
            }
        }
        vec![]
    }

    /// Get next nodes after executing the given nodes
    pub fn get_next_nodes(&self, executed: &[String], state: &State) -> Vec<String> {
        let mut next = Vec::new();

        for edge in &self.edges {
            match edge {
                Edge::Direct { source, target: EdgeTarget::Node(n) }
                    if executed.contains(source) =>
                {
                    if !next.contains(n) {
                        next.push(n.clone());
                    }
                }
                Edge::Conditional { source, router, targets } if executed.contains(source) => {
                    let route = router(state);
                    if let Some(EdgeTarget::Node(n)) = targets.get(&route) {
                        if !next.contains(n) {
                            next.push(n.clone());
                        }
                    }
                    // If route leads to END or not found in targets, next will be empty for this path
                }
                _ => {}
            }
        }

        next
    }

    /// Check if any of the executed nodes lead to END
    pub fn leads_to_end(&self, executed: &[String], state: &State) -> bool {
        for edge in &self.edges {
            match edge {
                Edge::Direct { source, target } if executed.contains(source) => {
                    if target.is_end() {
                        return true;
                    }
                }
                Edge::Conditional { source, router, targets } if executed.contains(source) => {
                    let route = router(state);
                    if route == END {
                        return true;
                    }
                    if let Some(target) = targets.get(&route) {
                        if target.is_end() {
                            return true;
                        }
                    }
                }
                _ => {}
            }
        }
        false
    }

    /// Get the state schema
    pub fn schema(&self) -> &StateSchema {
        &self.schema
    }

    /// Get the checkpointer if configured
    pub fn checkpointer(&self) -> Option<&Arc<dyn Checkpointer>> {
        self.checkpointer.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_basic_graph_construction() {
        let graph = StateGraph::with_channels(&["input", "output"])
            .add_node_fn("process", |_ctx| async { Ok(NodeOutput::new()) })
            .add_edge(START, "process")
            .add_edge("process", END)
            .compile();

        assert!(graph.is_ok());
    }

    #[test]
    fn test_graph_missing_entry() {
        let graph = StateGraph::with_channels(&["input"])
            .add_node_fn("process", |_ctx| async { Ok(NodeOutput::new()) })
            .add_edge("process", END) // No START -> process edge
            .compile();

        assert!(matches!(graph, Err(GraphError::NoEntryPoint)));
    }

    #[test]
    fn test_graph_missing_node() {
        let graph = StateGraph::with_channels(&["input"]).add_edge(START, "nonexistent").compile();

        assert!(matches!(graph, Err(GraphError::EdgeTargetNotFound(_))));
    }

    #[test]
    fn test_conditional_edges() {
        let graph = StateGraph::with_channels(&["next"])
            .add_node_fn("router", |_ctx| async { Ok(NodeOutput::new()) })
            .add_node_fn("path_a", |_ctx| async { Ok(NodeOutput::new()) })
            .add_node_fn("path_b", |_ctx| async { Ok(NodeOutput::new()) })
            .add_edge(START, "router")
            .add_conditional_edges(
                "router",
                |state| state.get("next").and_then(|v| v.as_str()).unwrap_or(END).to_string(),
                [("path_a", "path_a"), ("path_b", "path_b"), (END, END)],
            )
            .compile()
            .unwrap();

        // Test routing
        let mut state = State::new();
        state.insert("next".to_string(), json!("path_a"));
        let next = graph.get_next_nodes(&["router".to_string()], &state);
        assert_eq!(next, vec!["path_a".to_string()]);

        state.insert("next".to_string(), json!("path_b"));
        let next = graph.get_next_nodes(&["router".to_string()], &state);
        assert_eq!(next, vec!["path_b".to_string()]);
    }
}
