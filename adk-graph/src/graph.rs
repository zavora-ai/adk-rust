//! StateGraph builder for constructing graphs

use crate::checkpoint::Checkpointer;
use crate::deferred::DeferredNodeConfig;
use crate::edge::{END, Edge, EdgeTarget, RouterFn, START};
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
    /// Fan-in (deferred) node configurations, keyed by node name.
    pub deferred_configs: HashMap<String, DeferredNodeConfig>,
}

impl StateGraph {
    /// Create a new graph with the given state schema
    pub fn new(schema: StateSchema) -> Self {
        Self { schema, nodes: HashMap::new(), edges: vec![], deferred_configs: HashMap::new() }
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

    /// Add a **fan-in** (deferred) function node.
    ///
    /// Unlike [`add_node_fn`](Self::add_node_fn), a deferred node does not run as
    /// soon as one upstream edge completes — the scheduler holds it until **all**
    /// upstream paths that can reach it have finished (or, with a configured
    /// `fan_in_timeout`, until that deadline). This is what makes a fan-out /
    /// fan-in pattern correct: several branches run in parallel and a single
    /// aggregator node runs once, after they all complete.
    ///
    /// The [`DeferredNodeConfig`] selects how the upstream outputs are exposed
    /// (e.g. [`MergeStrategy::Collect`](crate::deferred::MergeStrategy::Collect))
    /// and an optional fan-in timeout.
    ///
    /// # Example
    /// ```ignore
    /// use adk_graph::{StateGraph, DeferredNodeConfig, MergeStrategy};
    /// let graph = StateGraph::with_channels(&["x"])
    ///     .add_node_fn("a", |_| async { Ok(Default::default()) })
    ///     .add_node_fn("b", |_| async { Ok(Default::default()) })
    ///     .add_deferred_node_fn("join", |_| async { Ok(Default::default()) },
    ///         DeferredNodeConfig { merge_strategy: MergeStrategy::Collect, ..Default::default() })
    ///     .add_edge("a", "join")
    ///     .add_edge("b", "join");
    /// ```
    pub fn add_deferred_node_fn<F, Fut>(
        mut self,
        name: &str,
        func: F,
        config: DeferredNodeConfig,
    ) -> Self
    where
        F: Fn(NodeContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<NodeOutput>> + Send + 'static,
    {
        self.deferred_configs.insert(name.to_string(), config);
        self.add_node(FunctionNode::new(name, func))
    }

    /// Mark an already-added node as a fan-in (deferred) node.
    ///
    /// Useful when the node was registered via [`add_node`](Self::add_node) with
    /// a custom [`Node`] implementation.
    pub fn mark_deferred(mut self, name: &str, config: DeferredNodeConfig) -> Self {
        self.deferred_configs.insert(name.to_string(), config);
        self
    }

    /// Add a direct edge from source to target
    pub fn add_edge(mut self, source: &str, target: &str) -> Self {
        let target = EdgeTarget::from(target);

        if source == START {
            // Find existing entry or create new one
            let entry_idx = self.edges.iter().position(|e| matches!(e, Edge::Entry { .. }));

            match entry_idx {
                Some(idx) => {
                    if let Edge::Entry { targets } = &mut self.edges[idx]
                        && let EdgeTarget::Node(node) = &target
                        && !targets.contains(node)
                    {
                        targets.push(node.clone());
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
    pub fn compile(mut self) -> Result<CompiledGraph> {
        // A node with requirements on the graph that holds it states them now, so
        // a mismatch cannot reach a run. `SubgraphNode` checks its channel map here.
        for node in self.nodes.values() {
            node.validate_against(&self.schema)?;
        }
        self.validate()?;
        self.defer_unconditional_fan_in();

        Ok(CompiledGraph {
            schema: self.schema,
            nodes: self.nodes,
            edges: self.edges,
            checkpointer: None,
            interrupt_before: HashSet::new(),
            interrupt_after: HashSet::new(),
            recursion_limit: 100,
            timeout_policies: HashMap::new(),
            default_timeout: None,
            default_retry: None,
            error_handlers: HashMap::new(),
            default_error_handler: None,
            deferred_configs: self.deferred_configs,
            max_concurrency: None,
            retry_policies: HashMap::new(),
            strict_channels: false,
            retention: None,
            #[cfg(feature = "node-cache")]
            cache_policies: HashMap::new(),
        })
    }

    /// Mark any node reached by more than one unconditional edge as deferred.
    ///
    /// The frontier advances from whichever nodes finished in the last
    /// super-step, so without this a join becomes eligible as soon as one
    /// predecessor lands. On branches of unequal length it then runs once per
    /// arriving predecessor, applying its updates repeatedly and reading a
    /// half-built state.
    ///
    /// Only `Direct` and `Entry` edges count. A conditional predecessor may never
    /// fire, and waiting for one that cannot arrive would deadlock the join. A
    /// graph whose fan-in arrives through conditional edges therefore still needs
    /// `mark_deferred` and a `fan_in_timeout`.
    ///
    /// An explicit configuration always wins, so a caller who wants the earlier
    /// behaviour keeps it by configuring the node themselves.
    fn defer_unconditional_fan_in(&mut self) {
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        for edge in &self.edges {
            match edge {
                Edge::Direct { target, .. } => {
                    if let Some(name) = target.node_name() {
                        *in_degree.entry(name).or_insert(0) += 1;
                    }
                }
                Edge::Entry { targets } => {
                    for target in targets {
                        *in_degree.entry(target.as_str()).or_insert(0) += 1;
                    }
                }
                // A conditional edge selects one target at run time, so its
                // targets are not guaranteed arrivals.
                Edge::Conditional { .. } => {}
            }
        }

        let fan_ins: Vec<String> = in_degree
            .into_iter()
            .filter(|(name, degree)| *degree > 1 && self.nodes.contains_key(*name))
            .map(|(name, _)| name.to_string())
            .collect();

        for name in fan_ins {
            self.deferred_configs.entry(name).or_default();
        }
    }

    /// Validate the graph structure
    fn validate(&self) -> Result<()> {
        // Check for entry point
        let has_entry = self.edges.iter().any(|e| matches!(e, Edge::Entry { .. }));
        if !has_entry {
            return Err(GraphError::NoEntryPoint);
        }

        // Reject a node that cannot execute. A configuration whose backend is
        // unavailable should fail here, not part-way through a run when earlier nodes
        // may already have had side effects.
        for node in self.nodes.values() {
            node.validate()?;
        }

        // Check all node references exist
        for edge in &self.edges {
            match edge {
                Edge::Direct { source, target } => {
                    if source != START && !self.nodes.contains_key(source) {
                        return Err(GraphError::NodeNotFound(source.clone()));
                    }
                    if let EdgeTarget::Node(name) = target
                        && !self.nodes.contains_key(name)
                    {
                        return Err(GraphError::EdgeTargetNotFound(name.clone()));
                    }
                }
                Edge::Conditional { source, targets, .. } => {
                    if !self.nodes.contains_key(source) {
                        return Err(GraphError::NodeNotFound(source.clone()));
                    }
                    for target in targets.values() {
                        if let EdgeTarget::Node(name) = target
                            && !self.nodes.contains_key(name)
                        {
                            return Err(GraphError::EdgeTargetNotFound(name.clone()));
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

/// Turns a node failure into state and a route, instead of ending the run.
///
/// Called after the node's retry budget is spent. Returning a
/// [`crate::node::NodeOutput`] lets the handler record what happened
/// and name a recovery node with
/// [`with_goto`](crate::node::NodeOutput::with_goto). Returning `Err` ends the
/// run as before.
pub type NodeErrorHandler =
    Arc<dyn Fn(&str, &GraphError, &State) -> Result<crate::node::NodeOutput> + Send + Sync>;

/// Policies a graph applies to every node that does not set its own.
///
/// Repeating the same retry or timeout on twenty nodes is easy to get wrong by
/// omission. A default states it once; a per-node value always wins.
///
/// # Example
///
/// ```
/// use adk_graph::graph::NodeDefaults;
/// use adk_graph::retry::RetryPolicy;
/// use adk_graph::timeout::TimeoutPolicy;
/// use std::time::Duration;
///
/// let defaults = NodeDefaults::new().with_retry(RetryPolicy::new(3)).with_timeout(
///     TimeoutPolicy { run_timeout: Some(Duration::from_secs(30)), ..Default::default() },
/// );
/// # let _ = defaults;
/// ```
#[derive(Clone, Default)]
pub struct NodeDefaults {
    /// Retry policy for a node with none of its own.
    pub retry: Option<crate::retry::RetryPolicy>,
    /// Timeout policy for a node with none of its own.
    pub timeout: Option<crate::timeout::TimeoutPolicy>,
    /// Failure handler for a node with none of its own.
    pub error_handler: Option<NodeErrorHandler>,
}

impl std::fmt::Debug for NodeDefaults {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeDefaults")
            .field("retry", &self.retry)
            .field("timeout", &self.timeout)
            .field("error_handler", &self.error_handler.as_ref().map(|_| "<handler>"))
            .finish()
    }
}

impl NodeDefaults {
    /// An empty set of defaults, which changes nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies this retry policy to every node that sets none.
    pub fn with_retry(mut self, policy: crate::retry::RetryPolicy) -> Self {
        self.retry = Some(policy);
        self
    }

    /// Applies this timeout policy to every node that sets none.
    pub fn with_timeout(mut self, policy: crate::timeout::TimeoutPolicy) -> Self {
        self.timeout = Some(policy);
        self
    }

    /// Applies this failure handler to every node that sets none.
    pub fn with_error_handler<F>(mut self, handler: F) -> Self
    where
        F: Fn(&str, &GraphError, &State) -> Result<crate::node::NodeOutput> + Send + Sync + 'static,
    {
        self.error_handler = Some(Arc::new(handler));
        self
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
    /// Per-node timeout policies, keyed by node name.
    pub(crate) timeout_policies: HashMap<String, crate::timeout::TimeoutPolicy>,
    /// Default timeout policy applied to all nodes without an explicit override.
    pub(crate) default_timeout: Option<crate::timeout::TimeoutPolicy>,
    /// Retry policy for every node that sets none of its own.
    pub(crate) default_retry: Option<crate::retry::RetryPolicy>,
    /// Per-node failure handlers, keyed by node name.
    pub(crate) error_handlers: HashMap<String, NodeErrorHandler>,
    /// Failure handler for every node that sets none of its own.
    pub(crate) default_error_handler: Option<NodeErrorHandler>,
    /// Deferred node configurations, keyed by node name.
    pub(crate) deferred_configs: HashMap<String, crate::deferred::DeferredNodeConfig>,
    /// Ceiling on how many nodes execute at once. `None` runs the whole frontier.
    pub(crate) max_concurrency: Option<usize>,
    /// Per-node retry policies, keyed by node name.
    pub(crate) retry_policies: HashMap<String, crate::retry::RetryPolicy>,
    /// Whether a node writing an undeclared channel fails the run.
    pub(crate) strict_channels: bool,
    /// How many checkpoints to keep per thread. `None` keeps every one.
    pub(crate) retention: Option<crate::checkpoint::RetentionPolicy>,
    /// Per-node cache policies, keyed by node name.
    #[cfg(feature = "node-cache")]
    pub(crate) cache_policies: HashMap<String, crate::cache::NodeCachePolicy>,
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

    /// Cap how many nodes execute concurrently within one super-step.
    ///
    /// A wide fan-out otherwise dispatches its whole frontier at once, which can
    /// exhaust a connection pool or trip a provider rate limit. Nodes beyond the
    /// cap wait for a slot; the dispatch order is the frontier's, sorted, so it
    /// does not depend on timing.
    ///
    /// Without this the frontier runs unbounded, which stays the default.
    pub fn with_max_concurrency(mut self, limit: usize) -> Self {
        self.max_concurrency = Some(limit.max(1));
        self
    }

    /// Fail the run when a node writes a channel the schema does not declare.
    ///
    /// An undeclared channel otherwise takes the overwrite reducer, because that
    /// is the fallback for a name the schema does not hold. A graph that declared
    /// a list channel and then wrote a near-miss name keeps only the last value
    /// and reports nothing. Enforcement turns that into
    /// [`crate::error::GraphError::UndeclaredChannel`].
    ///
    /// A graph that declares no channels accepts any name even under
    /// enforcement, because there is nothing to check against.
    ///
    /// Off by default: a graph may legitimately declare the channels a caller
    /// reads and let its nodes pass other values between themselves.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_graph::edge::{END, START};
    /// use adk_graph::graph::StateGraph;
    /// use adk_graph::node::NodeOutput;
    /// use serde_json::json;
    ///
    /// let graph = StateGraph::with_channels(&["total"])
    ///     .add_node_fn("sum", |_ctx| async move {
    ///         Ok(NodeOutput::new().with_update("total", json!(3)))
    ///     })
    ///     .add_edge(START, "sum")
    ///     .add_edge("sum", END)
    ///     .compile()
    ///     .unwrap()
    ///     .with_strict_channels();
    /// # let _ = graph;
    /// ```
    pub fn with_strict_channels(mut self) -> Self {
        self.strict_channels = true;
        self
    }

    /// Discards old checkpoints as the run proceeds.
    ///
    /// A thread otherwise accumulates one checkpoint per super-step for as long as
    /// it lives, which costs storage and slows a `list`. The newest is always kept,
    /// because it is the one a resume loads.
    ///
    /// Off by default, so an existing thread keeps its whole history and time
    /// travel can still reach every step.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_graph::checkpoint::{MemoryCheckpointer, RetentionPolicy};
    /// use adk_graph::edge::{END, START};
    /// use adk_graph::graph::StateGraph;
    /// use adk_graph::node::NodeOutput;
    ///
    /// let graph = StateGraph::with_channels(&["value"])
    ///     .add_node_fn("step", |_ctx| async move { Ok(NodeOutput::new()) })
    ///     .add_edge(START, "step")
    ///     .add_edge("step", END)
    ///     .compile()?
    ///     .with_checkpointer(MemoryCheckpointer::new())
    ///     .with_checkpoint_retention(RetentionPolicy::keep_last(20));
    /// # let _ = graph;
    /// # Ok::<(), adk_graph::error::GraphError>(())
    /// ```
    pub fn with_checkpoint_retention(mut self, policy: crate::checkpoint::RetentionPolicy) -> Self {
        self.retention = Some(policy);
        self
    }

    /// Applies policies to every node that does not set its own.
    ///
    /// Repeating the same retry or timeout across twenty nodes is easy to get
    /// wrong by omission. A per-node value always wins over the default.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_graph::edge::{END, START};
    /// use adk_graph::graph::{NodeDefaults, StateGraph};
    /// use adk_graph::node::NodeOutput;
    /// use adk_graph::retry::RetryPolicy;
    ///
    /// let graph = StateGraph::with_channels(&["value"])
    ///     .add_node_fn("fetch", |_ctx| async move { Ok(NodeOutput::new()) })
    ///     .add_edge(START, "fetch")
    ///     .add_edge("fetch", END)
    ///     .compile()?
    ///     // Every node retries three times, unless it says otherwise.
    ///     .with_node_defaults(NodeDefaults::new().with_retry(RetryPolicy::new(3)))
    ///     // And this one gets five.
    ///     .with_node_retry("fetch", RetryPolicy::new(5));
    /// # let _ = graph;
    /// # Ok::<(), adk_graph::error::GraphError>(())
    /// ```
    pub fn with_node_defaults(mut self, defaults: NodeDefaults) -> Self {
        if let Some(retry) = defaults.retry {
            self.default_retry = Some(retry);
        }
        if let Some(timeout) = defaults.timeout {
            self.default_timeout = Some(timeout);
        }
        if let Some(handler) = defaults.error_handler {
            self.default_error_handler = Some(handler);
        }
        self
    }

    /// Handles one node's failure instead of ending the run.
    ///
    /// Called once the node's retry budget is spent. The handler receives the node
    /// name, the error, and the state as it stands, and returns the updates to
    /// apply — typically recording what failed and naming a recovery node with
    /// [`NodeOutput::with_goto`](crate::node::NodeOutput::with_goto). Returning
    /// `Err` ends the run.
    ///
    /// An interrupt is never routed here: a pause is not a failure.
    pub fn with_node_error_handler<F>(mut self, node: &str, handler: F) -> Self
    where
        F: Fn(&str, &GraphError, &State) -> Result<crate::node::NodeOutput> + Send + Sync + 'static,
    {
        self.error_handlers.insert(node.to_string(), Arc::new(handler));
        self
    }

    /// Whether this graph holds a checkpointer.
    pub fn has_checkpointer(&self) -> bool {
        self.checkpointer.is_some()
    }

    /// Whether this graph declares any static interrupt gate.
    ///
    /// A dynamic interrupt cannot be seen from the graph, because a node decides
    /// at run time, so this reports only the declared gates.
    pub fn can_pause(&self) -> bool {
        !self.interrupt_before.is_empty() || !self.interrupt_after.is_empty()
    }

    /// The failure handler for a node, per-node first, then the graph default.
    pub(crate) fn error_handler_for(&self, node: &str) -> Option<&NodeErrorHandler> {
        self.error_handlers.get(node).or(self.default_error_handler.as_ref())
    }

    /// Attach a retry policy to one node.
    ///
    /// A node with no policy is attempted once, which is the behaviour of a graph
    /// that configures none.
    pub fn with_node_retry(mut self, node: &str, policy: crate::retry::RetryPolicy) -> Self {
        self.retry_policies.insert(node.to_string(), policy);
        self
    }

    /// A node by name, for a caller that needs to run one on its own.
    pub fn node(&self, name: &str) -> Option<Arc<dyn Node>> {
        self.nodes.get(name).cloned()
    }

    /// The declared state channel names, sorted.
    pub fn state_channels(&self) -> Vec<String> {
        let mut names: Vec<String> = self.schema.channels.keys().cloned().collect();
        names.sort();
        names
    }

    /// The retry policy for a node.
    ///
    /// The per-node policy wins; otherwise the graph's default applies. `None`
    /// when neither is set, which means one attempt.
    pub(crate) fn retry_policy_for(&self, node: &str) -> Option<&crate::retry::RetryPolicy> {
        self.retry_policies.get(node).or(self.default_retry.as_ref())
    }

    /// Get the effective timeout policy for a node.
    ///
    /// Returns the per-node policy if one was configured via
    /// `GraphAgentBuilder::node_timeout`, otherwise falls back to the
    /// default timeout policy. Returns `None` if neither is set.
    pub fn timeout_policy_for(&self, node_name: &str) -> Option<&crate::timeout::TimeoutPolicy> {
        self.timeout_policies.get(node_name).or(self.default_timeout.as_ref())
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
    /// # Errors
    ///
    /// Returns [`GraphError::UnknownRouteTarget`] when a router answers with a
    /// key that is not among the declared targets. A route to `END` is declared,
    /// so it is not an error; a key nobody declared is, because the branch would
    /// otherwise stop and the run would report success having skipped the work.
    pub fn get_next_nodes(&self, executed: &[String], state: &State) -> Result<Vec<String>> {
        let mut next = Vec::new();

        for edge in &self.edges {
            match edge {
                Edge::Direct { source, target: EdgeTarget::Node(n) }
                    if executed.contains(source) && !next.contains(n) =>
                {
                    next.push(n.clone());
                }
                Edge::Conditional { source, router, targets } if executed.contains(source) => {
                    let route = router(state);
                    match targets.get(&route) {
                        Some(EdgeTarget::Node(n)) if !next.contains(n) => next.push(n.clone()),
                        // Declared, and either already queued or the end of this branch.
                        Some(_) => {}
                        None => {
                            return Err(GraphError::UnknownRouteTarget(format!(
                                "node '{source}' routed to '{route}', which is not a declared target. Declared: {declared:?}",
                                declared = {
                                    let mut keys: Vec<&str> =
                                        targets.keys().map(String::as_str).collect();
                                    keys.sort_unstable();
                                    keys
                                }
                            )));
                        }
                    }
                }
                _ => {}
            }
        }

        Ok(next)
    }

    /// Reports the conditional dispatches the executed nodes produce.
    ///
    /// Only conditional edges appear: a direct edge involves no decision. Used
    /// for [`StreamEvent::RouteDispatched`](crate::stream::StreamEvent::RouteDispatched),
    /// and called only when a caller asked for the debug stream, so a router is
    /// not evaluated again on the common path.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::UnknownRouteTarget`] on an undeclared route key,
    /// matching [`Self::get_next_nodes`].
    pub fn route_dispatches(
        &self,
        executed: &[String],
        state: &State,
    ) -> Result<Vec<(String, Vec<String>)>> {
        let mut dispatches = Vec::new();
        for edge in &self.edges {
            if let Edge::Conditional { source, router, targets } = edge
                && executed.contains(source)
            {
                let route = router(state);
                match targets.get(&route) {
                    Some(EdgeTarget::Node(n)) => {
                        dispatches.push((source.clone(), vec![n.clone()]));
                    }
                    Some(_) => dispatches.push((source.clone(), Vec::new())),
                    None => {
                        return Err(GraphError::UnknownRouteTarget(format!(
                            "node '{source}' routed to '{route}', which is not a declared target"
                        )));
                    }
                }
            }
        }
        Ok(dispatches)
    }

    /// Check if any of the executed nodes lead to END
    pub fn leads_to_end(&self, executed: &[String], state: &State) -> bool {
        for edge in &self.edges {
            match edge {
                Edge::Direct { source, target } if executed.contains(source) && target.is_end() => {
                    return true;
                }
                Edge::Conditional { source, router, targets } if executed.contains(source) => {
                    let route = router(state);
                    if route == END {
                        return true;
                    }
                    if let Some(target) = targets.get(&route)
                        && target.is_end()
                    {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    /// Get all upstream source nodes for a given target node.
    ///
    /// Returns the names of all nodes that have an edge pointing to the given
    /// target node. This is used by the deferred node scheduler to determine
    /// which upstream paths must complete before a fan-in node can execute.
    ///
    /// For conditional edges, all possible source nodes are included since any
    /// of them could route to the target at runtime.
    pub fn get_upstream_nodes(&self, target_node: &str) -> Vec<String> {
        let mut sources = Vec::new();

        for edge in &self.edges {
            match edge {
                Edge::Direct { source, target } => {
                    if let EdgeTarget::Node(name) = target
                        && name == target_node
                        && !sources.contains(source)
                    {
                        sources.push(source.clone());
                    }
                }
                Edge::Conditional { source, targets, .. } => {
                    for target in targets.values() {
                        if let EdgeTarget::Node(name) = target
                            && name == target_node
                            && !sources.contains(source)
                        {
                            sources.push(source.clone());
                        }
                    }
                }
                Edge::Entry { targets } => {
                    if targets.contains(&target_node.to_string()) {
                        // Entry nodes come from START, which is not a real node
                        // so we don't add it as an upstream source
                    }
                }
            }
        }

        sources
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
        let next = graph.get_next_nodes(&["router".to_string()], &state).unwrap();
        assert_eq!(next, vec!["path_a".to_string()]);

        state.insert("next".to_string(), json!("path_b"));
        let next = graph.get_next_nodes(&["router".to_string()], &state).unwrap();
        assert_eq!(next, vec!["path_b".to_string()]);
    }
}
