//! Node types for graph execution
//!
//! Nodes are the computational units in a graph. They receive state and return updates.

use crate::error::Result;
use crate::interrupt::Interrupt;
use crate::state::State;
use crate::stream::StreamEvent;
use crate::timeout::ProgressHandle;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Configuration passed to nodes during execution
#[derive(Clone)]
pub struct ExecutionConfig {
    /// Thread identifier for checkpointing
    pub thread_id: String,
    /// Resume from a specific checkpoint
    pub resume_from: Option<String>,
    /// Answers to pauses, keyed by what paused.
    ///
    /// | Key | Who reads the value |
    /// |-----|---------------------|
    /// | A continuation key such as `interrupt-1` | The `#[entrypoint]` interrupt site with that key, which returns it instead of suspending |
    /// | The name of a graph node that paused with a dynamic interrupt | That node, through [`NodeContext::resume_value`], in the step that resumes it |
    ///
    /// A graph node's answer is not written to state, so it answers one pause: a
    /// node that runs again later, such as in the next pass of a loop or on the
    /// next turn of a [`GraphAgent`](crate::agent::GraphAgent), pauses again.
    pub resume_values: HashMap<String, Value>,
    /// How many super-steps one run may take before it fails with
    /// [`GraphError::RecursionLimitExceeded`](crate::error::GraphError::RecursionLimitExceeded).
    ///
    /// `None` uses the graph's own limit, set with
    /// [`CompiledGraph::with_recursion_limit`](crate::graph::CompiledGraph::with_recursion_limit)
    /// or [`GraphAgentBuilder::recursion_limit`](crate::agent::GraphAgentBuilder::recursion_limit)
    /// (100 when neither is set); a value here overrides it for this run.
    pub recursion_limit: Option<usize>,
    /// Additional configuration
    pub metadata: HashMap<String, Value>,
    /// The invocation this graph run belongs to, when it has one.
    ///
    /// An [`AgentNode`] runs a real agent, and that agent expects the identity,
    /// services, and cancellation of the run it belongs to. Without a parent the node
    /// has to fabricate them, which makes an agent behave differently inside a graph
    /// than outside it. Set this with
    /// [`ExecutionConfig::with_parent_context`] to carry them through; leaving it
    /// unset is standalone mode and is what a graph invoked outside a `Runner` gets.
    pub parent_context: Option<Arc<dyn adk_core::InvocationContext>>,
    /// When the run writes each super-step's checkpoint. Defaults to
    /// [`Durability::Sync`](crate::checkpoint::Durability::Sync).
    pub durability: crate::checkpoint::Durability,
    /// Cancels a functional-API run.
    ///
    /// An `#[entrypoint]` run hands this token to its `TaskContext` and stops when
    /// it is cancelled: the task running at that moment is abandoned, no later task
    /// starts, and the run returns
    /// [`GraphError::Cancelled`](crate::error::GraphError::Cancelled). Unset, the
    /// run gets a token nobody else holds.
    #[cfg(feature = "functional")]
    pub cancellation_token: Option<tokio_util::sync::CancellationToken>,
    /// Where agent nodes forward their agents' events as the agents produce them.
    ///
    /// Set by [`GraphAgent`](crate::agent::GraphAgent), which streams these events
    /// to its caller, so a runner shows and records them while the graph runs.
    pub(crate) event_sink: Option<tokio::sync::mpsc::UnboundedSender<adk_core::Event>>,
    /// The lineage root a subgraph's run keys its governed nodes' records by, derived from
    /// the parent's, so a fork of the parent finds the records its source wrote.
    pub(crate) lineage_root: Option<String>,
}

impl ExecutionConfig {
    /// Create a new config with the given thread ID
    pub fn new(thread_id: &str) -> Self {
        Self {
            thread_id: thread_id.to_string(),
            resume_from: None,
            resume_values: HashMap::new(),
            recursion_limit: None,
            metadata: HashMap::new(),
            parent_context: None,
            durability: crate::checkpoint::Durability::default(),
            #[cfg(feature = "functional")]
            cancellation_token: None,
            event_sink: None,
            lineage_root: None,
        }
    }

    /// Lets the caller cancel a functional-API run with `token`.
    ///
    /// See [`ExecutionConfig::cancellation_token`].
    ///
    /// # Example
    ///
    /// ```
    /// use adk_graph::node::ExecutionConfig;
    /// use tokio_util::sync::CancellationToken;
    ///
    /// let token = CancellationToken::new();
    /// let config = ExecutionConfig::new("triage-7").with_cancellation_token(token.clone());
    /// token.cancel();
    /// assert!(config.cancellation_token.is_some_and(|token| token.is_cancelled()));
    /// ```
    #[cfg(feature = "functional")]
    #[must_use]
    pub fn with_cancellation_token(mut self, token: tokio_util::sync::CancellationToken) -> Self {
        self.cancellation_token = Some(token);
        self
    }

    /// Forwards agent nodes' events to `sink` as their agents produce them.
    pub(crate) fn with_event_sink(
        mut self,
        sink: tokio::sync::mpsc::UnboundedSender<adk_core::Event>,
    ) -> Self {
        self.event_sink = Some(sink);
        self
    }

    /// Sets when the run writes each super-step's checkpoint.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_graph::checkpoint::Durability;
    /// use adk_graph::node::ExecutionConfig;
    ///
    /// let config = ExecutionConfig::new("thread").with_durability(Durability::Exit);
    /// assert_eq!(config.durability, Durability::Exit);
    /// ```
    pub fn with_durability(mut self, durability: crate::checkpoint::Durability) -> Self {
        self.durability = durability;
        self
    }

    /// Carry the invocation this graph run belongs to into its nodes.
    ///
    /// An [`AgentNode`] then presents the caller's identity, services, request
    /// context, and cancellation to the agent it runs, instead of a synthetic
    /// standalone context.
    #[must_use]
    pub fn with_parent_context(mut self, parent: Arc<dyn adk_core::InvocationContext>) -> Self {
        self.parent_context = Some(parent);
        self
    }

    /// Overrides the graph's recursion limit for this run.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_graph::node::ExecutionConfig;
    ///
    /// let config = ExecutionConfig::new("thread").with_recursion_limit(500);
    /// assert_eq!(config.recursion_limit, Some(500));
    /// assert_eq!(ExecutionConfig::new("thread").recursion_limit, None);
    /// ```
    pub fn with_recursion_limit(mut self, limit: usize) -> Self {
        self.recursion_limit = Some(limit);
        self
    }

    /// Resume from a specific checkpoint
    pub fn with_resume_from(mut self, checkpoint_id: &str) -> Self {
        self.resume_from = Some(checkpoint_id.to_string());
        self
    }

    /// Supplies the answer to a pause.
    ///
    /// `key` is the key a suspended `#[entrypoint]` run reported, such as
    /// `interrupt-1`, or the name of a graph node that paused with a dynamic
    /// interrupt. See [`ExecutionConfig::resume_values`].
    ///
    /// # Example
    ///
    /// ```
    /// use adk_graph::node::ExecutionConfig;
    /// use serde_json::json;
    ///
    /// // A functional-API interrupt site.
    /// let config = ExecutionConfig::new("refund-42")
    ///     .with_resume_from("checkpoint-id")
    ///     .with_resume_value("interrupt-1", json!({ "approved": true }));
    /// assert_eq!(config.resume_values.get("interrupt-1"), Some(&json!({ "approved": true })));
    ///
    /// // A graph node named `review` that paused.
    /// let config = ExecutionConfig::new("refund-42").with_resume_value("review", json!("approve"));
    /// assert_eq!(config.resume_values.get("review"), Some(&json!("approve")));
    /// ```
    #[must_use]
    pub fn with_resume_value(mut self, key: impl Into<String>, value: Value) -> Self {
        self.resume_values.insert(key.into(), value);
        self
    }

    /// Add metadata
    pub fn with_metadata(mut self, key: &str, value: Value) -> Self {
        self.metadata.insert(key.to_string(), value);
        self
    }
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self::new(&uuid::Uuid::new_v4().to_string())
    }
}

/// Context passed to nodes during execution
#[derive(Clone)]
pub struct NodeContext {
    /// Current graph state (read-only view)
    pub state: State,
    /// Configuration for this execution
    pub config: ExecutionConfig,
    /// Current step number
    pub step: usize,
    /// Optional progress handle for idle timeout tracking.
    /// When present, calling [`report_progress()`](Self::report_progress) resets the idle timeout counter.
    progress_handle: Option<ProgressHandle>,
    /// Set by the executor when this node may invoke other nodes.
    children: Option<std::sync::Arc<crate::child::ChildInvoker>>,
    /// The schema of the graph running this node, for a node that projects state.
    parent_schema: Option<std::sync::Arc<crate::state::StateSchema>>,
    /// ADK configuration supplied through an additive `CompiledGraph` API.
    run_config: Option<adk_core::RunConfig>,
    /// The answer to this node's own pause, in the step that resumes it.
    resume_value: Option<Value>,
    /// Whether the node runs through the governed bracket, which sends its call once.
    governed: bool,
}

impl NodeContext {
    /// Create a new node context
    pub fn new(state: State, config: ExecutionConfig, step: usize) -> Self {
        Self {
            state,
            config,
            step,
            progress_handle: None,
            children: None,
            parent_schema: None,
            run_config: None,
            resume_value: None,
            governed: false,
        }
    }

    /// Whether this execution is a governed node's call, which runs once: an action node
    /// neither retries it nor hides its failure (spec 3.3 §12).
    pub(crate) fn governed_call(&self) -> bool {
        self.governed
    }

    /// Marks this execution as a governed node's call.
    pub(crate) fn set_governed_call(&mut self) {
        self.governed = true;
    }

    /// The answer to this node's pause, when this execution resumes it.
    ///
    /// Present only in the step that resumes a node that paused with a dynamic
    /// interrupt, and only when the caller supplied an answer under the node's
    /// name with [`ExecutionConfig::with_resume_value`]. A
    /// [`GraphAgent`](crate::agent::GraphAgent) supplies the turn's text. The
    /// answer is not written to state, so the next pause asks again.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_graph::node::{NodeContext, NodeOutput};
    ///
    /// async fn review(ctx: NodeContext) -> adk_graph::Result<NodeOutput> {
    ///     match ctx.resume_value().and_then(|answer| answer.as_str()) {
    ///         Some("approve") => Ok(NodeOutput::new()),
    ///         Some(_) => Ok(NodeOutput::new().with_goto(["rejected"])),
    ///         None => Ok(NodeOutput::interrupt("approve this refund?")),
    ///     }
    /// }
    /// ```
    pub fn resume_value(&self) -> Option<&Value> {
        self.resume_value.as_ref()
    }

    /// Attaches the answer to this node's pause.
    pub(crate) fn set_resume_value(&mut self, value: Value) {
        self.resume_value = Some(value);
    }

    /// The machinery for invoking other nodes, if this context has it.
    /// The schema of the graph running this node.
    ///
    /// Attached by the executor. A node that projects state between two schemas
    /// needs it; most nodes do not.
    pub fn parent_schema(&self) -> Option<std::sync::Arc<crate::state::StateSchema>> {
        self.parent_schema.clone()
    }

    /// Attaches the running graph's schema.
    pub fn set_parent_schema(&mut self, schema: std::sync::Arc<crate::state::StateSchema>) {
        self.parent_schema = Some(schema);
    }

    /// Attach per-run ADK configuration for an agent node.
    pub(crate) fn set_run_config(&mut self, run_config: adk_core::RunConfig) {
        self.run_config = Some(run_config);
    }

    pub(crate) fn run_config(&self) -> Option<adk_core::RunConfig> {
        self.run_config.clone()
    }

    /// Who the run executing this node acts as, and for whom.
    ///
    /// `Some` when the graph runs with a [`RunConfig`](adk_core::RunConfig), such as a
    /// graph agent under a runner, or a call to
    /// [`CompiledGraph::invoke_with_run_config`](crate::CompiledGraph::invoke_with_run_config).
    /// Agent nodes give their agents the resolved context through
    /// [`ReadonlyContext::acting_context`](adk_core::ReadonlyContext::acting_context).
    pub fn acting(&self) -> Option<&adk_core::ActingConfig> {
        self.run_config.as_ref().map(|config| &config.acting)
    }

    pub(crate) fn child_invoker(&self) -> Option<std::sync::Arc<crate::child::ChildInvoker>> {
        self.children.clone()
    }

    /// Attach the machinery for invoking other nodes.
    pub(crate) fn set_child_invoker(
        &mut self,
        invoker: std::sync::Arc<crate::child::ChildInvoker>,
    ) {
        self.children = Some(invoker);
    }

    /// Invoke another node and await its output.
    ///
    /// The child sees this node's state with `input` merged over it, and returns
    /// its updates as one object. Nothing is applied to the graph's state: the
    /// caller decides what to do with the result.
    ///
    /// A child that already completed under the same identity is not run again
    /// after a resume. See [`crate::child`] for how that identity is formed, and
    /// why a resumable parent should pass its own run id.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::NodeNotFound`](crate::error::GraphError::NodeNotFound)
    /// when no node has that name, whatever the child returns, and
    /// [`GraphError::Interrupted`](crate::error::GraphError::Interrupted) when the
    /// child pauses.
    pub async fn run_node(&self, child: &str, input: Value) -> Result<Value> {
        self.run_node_with(child, input, crate::child::RunNodeOptions::default()).await
    }

    /// Invoke another node with an explicit run id.
    ///
    /// # Errors
    ///
    /// As [`run_node`](Self::run_node), and additionally when this node was not
    /// given the ability to invoke children.
    pub async fn run_node_with(
        &self,
        child: &str,
        input: Value,
        options: crate::child::RunNodeOptions,
    ) -> Result<Value> {
        let invoker = self.children.as_ref().ok_or_else(|| {
            crate::error::GraphError::InvalidGraph(
                "this node cannot invoke other nodes: no child invoker was attached".to_string(),
            )
        })?;
        invoker.run(child, input, options, self).await
    }

    /// Get a value from state
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.state.get(key)
    }

    /// Get a value from state as a specific type
    pub fn get_as<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.state.get(key).and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// Report progress, resetting the idle timeout counter.
    ///
    /// Nodes performing long-running work should call this periodically to
    /// prevent the idle timeout from firing. If no progress handle is attached
    /// (e.g., when no idle timeout is configured), this is a no-op.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput> {
    ///     for chunk in large_dataset.chunks(100) {
    ///         process(chunk).await;
    ///         ctx.report_progress(); // reset idle timeout
    ///     }
    ///     Ok(NodeOutput::new())
    /// }
    /// ```
    pub fn report_progress(&self) {
        if let Some(handle) = &self.progress_handle {
            handle.report_progress();
        }
    }

    /// Attach a progress handle for idle timeout tracking.
    ///
    /// This is called by the executor before running a node with an idle timeout
    /// policy. Nodes do not need to call this directly.
    pub fn set_progress_handle(&mut self, handle: ProgressHandle) {
        self.progress_handle = Some(handle);
    }

    /// Get a reference to the attached progress handle, if any.
    pub fn progress_handle(&self) -> Option<&ProgressHandle> {
        self.progress_handle.as_ref()
    }
}

/// Output from a node execution
#[derive(Default)]
pub struct NodeOutput {
    /// State updates to apply
    pub updates: HashMap<String, Value>,
    /// Optional interrupt request
    pub interrupt: Option<Interrupt>,
    /// Custom stream events
    pub events: Vec<StreamEvent>,
    /// Nodes to run next, replacing this node's declared outgoing edges.
    pub goto: Option<Vec<String>>,
    /// Nodes of the *parent* graph to run next, when this graph is a subgraph.
    ///
    /// A node deep in a nested graph can end its own graph and hand control to a
    /// node of the graph that holds it.
    pub goto_parent: Option<Vec<String>>,
}

impl NodeOutput {
    /// Create a new empty output
    pub fn new() -> Self {
        Self::default()
    }

    /// Names the nodes to run next, replacing this node's declared edges.
    ///
    /// A conditional edge fixes its targets when the graph is built. This does
    /// not: a node reads state and names any node in the graph, including one it
    /// has no edge to. Naming [`END`](crate::edge::END) stops the branch.
    ///
    /// The declared edges from this node do not also fire. Setting no goto leaves
    /// the declared edges in charge, which is the default.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_graph::node::NodeOutput;
    /// use serde_json::json;
    ///
    /// // Write state and choose the next node in one step.
    /// let output = NodeOutput::new()
    ///     .with_update("risk", json!("high"))
    ///     .with_goto(["escalate"]);
    /// assert_eq!(output.goto.as_deref(), Some(&["escalate".to_string()][..]));
    /// ```
    /// Names nodes of the *parent* graph to run next.
    ///
    /// Only meaningful inside a [`SubgraphNode`](crate::subgraph::SubgraphNode).
    /// The subgraph finishes, its output channels are projected out as usual, and
    /// the parent continues at the named nodes rather than following the
    /// subgraph node's own edges. This is the counterpart to LangGraph's
    /// `Command(goto=..., graph=Command.PARENT)`.
    ///
    /// A name the parent does not hold fails the run with
    /// [`GraphError::UnknownRouteTarget`](crate::error::GraphError::UnknownRouteTarget),
    /// checked by the parent, which is the only side that knows its own nodes.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_graph::node::NodeOutput;
    /// use serde_json::json;
    ///
    /// // Inside a subgraph: give up, and let the parent's escalation path run.
    /// let output = NodeOutput::new()
    ///     .with_update("reason", json!("no confident answer"))
    ///     .with_goto_parent(["escalate"]);
    /// assert_eq!(output.goto_parent.as_deref(), Some(&["escalate".to_string()][..]));
    /// ```
    pub fn with_goto_parent<I, S>(mut self, targets: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.goto_parent = Some(targets.into_iter().map(Into::into).collect());
        self
    }

    pub fn with_goto<I, S>(mut self, targets: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.goto = Some(targets.into_iter().map(Into::into).collect());
        self
    }

    /// Add a state update
    pub fn with_update(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.updates.insert(key.to_string(), value.into());
        self
    }

    /// Add multiple state updates
    pub fn with_updates(mut self, updates: HashMap<String, Value>) -> Self {
        self.updates.extend(updates);
        self
    }

    /// Set an interrupt
    pub fn with_interrupt(mut self, interrupt: Interrupt) -> Self {
        self.interrupt = Some(interrupt);
        self
    }

    /// Add a custom stream event
    pub fn with_event(mut self, event: StreamEvent) -> Self {
        self.events.push(event);
        self
    }

    /// Create output that triggers a dynamic interrupt
    pub fn interrupt(message: &str) -> Self {
        Self::new().with_interrupt(crate::interrupt::interrupt(message))
    }

    /// Create output that triggers a dynamic interrupt with data
    pub fn interrupt_with_data(message: &str, data: Value) -> Self {
        Self::new().with_interrupt(crate::interrupt::interrupt_with_data(message, data))
    }
}

/// A node in the graph
#[async_trait]
pub trait Node: Send + Sync {
    /// Node identifier
    fn name(&self) -> &str;

    /// Human-readable purpose shown by generic workflow inspectors.
    fn description(&self) -> &str {
        "Graph workflow node"
    }

    /// Runtime capabilities inherited by portable graph topology metadata.
    fn capabilities(&self) -> adk_core::AgentCapabilities {
        adk_core::AgentCapabilities::default()
    }

    /// What kind of step this node is, as a workflow observer labels it in
    /// [`CompiledGraph::shape`](crate::graph::CompiledGraph::shape) (spec 3.3 §12).
    ///
    /// Defaults to [`NodeKind::Other`](crate::shape::NodeKind::Other).
    ///
    /// # Example
    ///
    /// ```
    /// use adk_graph::node::{FunctionNode, Node, NodeOutput};
    /// use adk_graph::shape::NodeKind;
    ///
    /// let node = FunctionNode::new("receipt", |_ctx| async { Ok(NodeOutput::new()) });
    /// assert_eq!(node.kind(), NodeKind::Function);
    /// ```
    fn kind(&self) -> crate::shape::NodeKind {
        crate::shape::NodeKind::Other
    }

    /// The call this node makes, as an approver sees it and as its record binds it, when
    /// the node is governed (spec 3.3 §12, §7.1).
    ///
    /// The value is shown in full on the decision and its
    /// [`json_digest`](adk_core::json_digest) is part of the call's action-ledger key, so a
    /// fork whose call is unchanged is answered from the record. `None`, the default,
    /// binds the call to the whole state the node reads.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_graph::node::{Node, NodeContext, NodeOutput};
    /// use serde_json::{Value, json};
    ///
    /// struct Charge;
    ///
    /// #[async_trait::async_trait]
    /// impl Node for Charge {
    ///     fn name(&self) -> &str {
    ///         "pay"
    ///     }
    ///     fn call_args(&self, ctx: &NodeContext) -> Option<Value> {
    ///         Some(json!({ "amountCents": ctx.get("amountCents").cloned()? }))
    ///     }
    ///     async fn execute(&self, _ctx: &NodeContext) -> adk_graph::Result<NodeOutput> {
    ///         Ok(NodeOutput::new())
    ///     }
    /// }
    /// ```
    fn call_args(&self, _ctx: &NodeContext) -> Option<Value> {
        None
    }

    /// Whether this node acts in `ctx`, asked before a governed node's call is authorized
    /// (spec 3.3 §12, §7.4).
    ///
    /// `Ok(false)` means the node does nothing here, such as an action node whose condition
    /// is false: a governed node then completes with no writes, and no decision is created
    /// for a call that would not run. An error means the node cannot act, such as an HTTP
    /// node whose request cannot be built: a governed node then fails with that error
    /// without asking anyone. The default is `Ok(true)`.
    ///
    /// # Errors
    ///
    /// Returns why the node cannot act in `ctx`.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_graph::node::{Node, NodeContext, NodeOutput};
    ///
    /// struct Refund;
    ///
    /// #[async_trait::async_trait]
    /// impl Node for Refund {
    ///     fn name(&self) -> &str {
    ///         "refund"
    ///     }
    ///     fn will_act(&self, ctx: &NodeContext) -> adk_graph::Result<bool> {
    ///         Ok(ctx.get("refund_due").and_then(|due| due.as_bool()).unwrap_or(false))
    ///     }
    ///     async fn execute(&self, _ctx: &NodeContext) -> adk_graph::Result<NodeOutput> {
    ///         Ok(NodeOutput::new())
    ///     }
    /// }
    /// ```
    fn will_act(&self, _ctx: &NodeContext) -> Result<bool> {
        Ok(true)
    }

    /// The graph this node runs, when it is a subgraph, so a workflow observer finds the
    /// governed nodes inside it (spec 3.3 §12). The default is `None`.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_graph::node::{FunctionNode, Node, NodeOutput};
    ///
    /// let node = FunctionNode::new("receipt", |_ctx| async { Ok(NodeOutput::new()) });
    /// assert!(node.subgraph().is_none());
    /// ```
    fn subgraph(&self) -> Option<&crate::graph::CompiledGraph> {
        None
    }

    /// Runs exactly `args`: an approver's edit of [`Node::call_args`] (spec 3.3 §7.1, §7.5).
    ///
    /// Called instead of [`Node::execute`] when an approver edited a governed node's call.
    /// It must send the call it is given and nothing else.
    ///
    /// # Errors
    ///
    /// The default returns [`GovernedError::EditUnsupported`](crate::governed::GovernedError::EditUnsupported):
    /// the node cannot run an edited call, so the edited call is refused and nothing runs.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_graph::node::{Node, NodeContext, NodeOutput};
    /// use serde_json::Value;
    ///
    /// struct Charge;
    ///
    /// #[async_trait::async_trait]
    /// impl Node for Charge {
    ///     fn name(&self) -> &str {
    ///         "pay"
    ///     }
    ///     async fn execute(&self, _ctx: &NodeContext) -> adk_graph::Result<NodeOutput> {
    ///         Ok(NodeOutput::new())
    ///     }
    ///     async fn run_call(&self, _ctx: &NodeContext, args: Value) -> adk_graph::Result<NodeOutput> {
    ///         Ok(NodeOutput::new().with_update("charged", args["amountCents"].clone()))
    ///     }
    /// }
    /// ```
    async fn run_call(&self, _ctx: &NodeContext, _args: Value) -> Result<NodeOutput> {
        Err(crate::governed::GovernedError::EditUnsupported { node: self.name().to_string() }
            .into())
    }

    /// Execute the node and return state updates
    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput>;

    /// Rejects a node that cannot execute, before the graph runs.
    ///
    /// Called for every node by [`StateGraph::compile`](crate::graph::StateGraph::compile), so a configuration whose
    /// backend is unavailable fails while the graph is being built rather than
    /// part-way through a run, when earlier nodes may already have had side effects.
    ///
    /// # Errors
    ///
    /// Returns an error describing what is unavailable. The default accepts the node.
    /// Checks this node against the schema of the graph that holds it.
    ///
    /// Called by [`StateGraph::compile`](crate::graph::StateGraph::compile) for
    /// every node, so a node that has requirements on its parent states them
    /// before anything runs. Defaults to accepting any parent.
    ///
    /// [`SubgraphNode`](crate::subgraph::SubgraphNode) uses this to reject a
    /// channel mapping that names a channel neither side declares.
    fn validate_against(&self, _parent: &crate::state::StateSchema) -> Result<()> {
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        Ok(())
    }

    /// Streams execution events for this node.
    ///
    /// An implementation must report the node's state updates by yielding a
    /// [`StreamEvent::Updates`] event, because a streaming executor takes the
    /// updates from this stream rather than executing the node a second time.
    /// The default implementation wraps [`Node::execute`] and does so.
    fn execute_stream<'a>(
        &'a self,
        ctx: &'a NodeContext,
    ) -> Pin<Box<dyn futures::Stream<Item = Result<StreamEvent>> + Send + 'a>> {
        let name = self.name().to_string();
        Box::pin(async_stream::stream! {
            match self.execute(ctx).await {
                Ok(output) => {
                    for event in output.events {
                        yield Ok(event);
                    }
                    // A goto and an interrupt have no other way through: this path
                    // yields events, not a NodeOutput, so the executor reads both
                    // back off the stream.
                    if let Some(targets) = output.goto {
                        yield Ok(StreamEvent::route_dispatched(&name, targets));
                    }
                    if let Some(interrupt) = output.interrupt {
                        let (message, data) = match interrupt {
                            crate::interrupt::Interrupt::Dynamic { message, data } => (message, data),
                            other => (other.to_string(), None),
                        };
                        yield Ok(StreamEvent::node_interrupt(&name, &message, data));
                    }
                    yield Ok(StreamEvent::Updates { node: name, updates: output.updates });
                }
                Err(e) => yield Err(e),
            }
        })
    }
}

/// Type alias for boxed node
pub type BoxedNode = Box<dyn Node>;

/// Type alias for async function signature
pub type AsyncNodeFn = Box<
    dyn Fn(NodeContext) -> Pin<Box<dyn Future<Output = Result<NodeOutput>> + Send>> + Send + Sync,
>;

/// Function node - wraps an async function as a node
pub struct FunctionNode {
    name: String,
    func: AsyncNodeFn,
}

impl FunctionNode {
    /// Create a new function node
    pub fn new<F, Fut>(name: &str, func: F) -> Self
    where
        F: Fn(NodeContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<NodeOutput>> + Send + 'static,
    {
        Self { name: name.to_string(), func: Box::new(move |ctx| Box::pin(func(ctx))) }
    }
}

#[async_trait]
impl Node for FunctionNode {
    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> crate::shape::NodeKind {
        crate::shape::NodeKind::Function
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput> {
        // The closure takes an owned context, so everything the executor attached
        // is carried across by cloning it.
        (self.func)(ctx.clone()).await
    }
}

/// Passthrough node - just passes state through unchanged
pub struct PassthroughNode {
    name: String,
}

impl PassthroughNode {
    /// Create a new passthrough node
    pub fn new(name: &str) -> Self {
        Self { name: name.to_string() }
    }
}

#[async_trait]
impl Node for PassthroughNode {
    fn name(&self) -> &str {
        &self.name
    }

    async fn execute(&self, _ctx: &NodeContext) -> Result<NodeOutput> {
        Ok(NodeOutput::new())
    }
}

/// Type alias for agent node input mapper
pub type AgentInputMapper = Box<dyn Fn(&State) -> adk_core::Content + Send + Sync>;

/// Type alias for agent node output mapper
pub type AgentOutputMapper =
    Box<dyn Fn(&[adk_core::Event]) -> HashMap<String, Value> + Send + Sync>;

/// Chooses an [`AgentNode`]'s successors from the updates it just produced.
///
/// Returning `None` leaves the node's declared edges in charge.
pub type AgentGotoMapper =
    Box<dyn Fn(&HashMap<String, Value>) -> Option<Vec<String>> + Send + Sync>;

/// Wrapper to use an existing ADK Agent as a graph node
pub struct AgentNode {
    name: String,
    #[allow(dead_code)]
    agent: Arc<dyn adk_core::Agent>,
    /// Map state to agent input content
    input_mapper: AgentInputMapper,
    /// Map agent events to state updates
    output_mapper: AgentOutputMapper,
    /// Choose successors from the mapped updates, replacing declared edges.
    goto_mapper: Option<AgentGotoMapper>,
}

impl AgentNode {
    /// Create a new agent node
    pub fn new(agent: Arc<dyn adk_core::Agent>) -> Self {
        let name = agent.name().to_string();
        Self {
            name,
            agent,
            input_mapper: Box::new(default_input_mapper),
            output_mapper: Box::new(default_output_mapper),
            goto_mapper: None,
        }
    }

    /// Set custom input mapper
    pub fn with_input_mapper<F>(mut self, mapper: F) -> Self
    where
        F: Fn(&State) -> adk_core::Content + Send + Sync + 'static,
    {
        self.input_mapper = Box::new(mapper);
        self
    }

    /// Set custom output mapper
    pub fn with_output_mapper<F>(mut self, mapper: F) -> Self
    where
        F: Fn(&[adk_core::Event]) -> HashMap<String, Value> + Send + Sync + 'static,
    {
        self.output_mapper = Box::new(mapper);
        self
    }

    /// Chooses this node's successors from the updates the output mapper produced.
    ///
    /// An agent's answer often decides where control goes next. The output mapper
    /// turns the agent's events into state; this turns that state into a route,
    /// so the classification is parsed once.
    ///
    /// Returning `None` leaves the declared edges in charge. Returning targets
    /// replaces them, exactly as [`NodeOutput::with_goto`] does for a plain node.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use adk_graph::node::AgentNode;
    /// # use std::collections::HashMap;
    /// # fn wire(node: AgentNode) -> AgentNode {
    /// node.with_goto_mapper(|updates: &HashMap<String, serde_json::Value>| {
    ///     match updates.get("category").and_then(|v| v.as_str()) {
    ///         Some("refund") => Some(vec!["refund_desk".to_string()]),
    ///         Some(_) => Some(vec!["general_desk".to_string()]),
    ///         None => None,
    ///     }
    /// })
    /// # }
    /// ```
    pub fn with_goto_mapper<F>(mut self, mapper: F) -> Self
    where
        F: Fn(&HashMap<String, Value>) -> Option<Vec<String>> + Send + Sync + 'static,
    {
        self.goto_mapper = Some(Box::new(mapper));
        self
    }
}

/// Default input mapper - looks for "messages" or "input" in state
fn default_input_mapper(state: &State) -> adk_core::Content {
    // Try to get messages first
    if let Some(messages) = state.get("messages")
        && let Some(arr) = messages.as_array()
        && let Some(last) = arr.last()
        && let Some(content) = last.get("content").and_then(|c| c.as_str())
    {
        return adk_core::Content::new("user").with_text(content);
    }

    // Try input field
    if let Some(input) = state.get("input")
        && let Some(text) = input.as_str()
    {
        return adk_core::Content::new("user").with_text(text);
    }

    adk_core::Content::new("user")
}

/// Default output mapper - extracts text content to "messages"
fn default_output_mapper(events: &[adk_core::Event]) -> HashMap<String, Value> {
    let mut updates = HashMap::new();

    // Collect text from events
    let mut messages = Vec::new();
    let mut text_deltas = adk_core::EventTextDeltas::default();
    for event in events {
        let event = text_deltas.push(event);
        if let Some(content) = event.content() {
            let text = content.parts.iter().filter_map(|p| p.text()).collect::<Vec<_>>().join("");

            if !text.is_empty() {
                messages.push(serde_json::json!({
                    "role": "assistant",
                    "content": text
                }));
            }
        }
    }

    if !messages.is_empty() {
        updates.insert("messages".to_string(), serde_json::json!(messages));
    }

    updates
}

#[async_trait]
impl Node for AgentNode {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        self.agent.description()
    }

    fn capabilities(&self) -> adk_core::AgentCapabilities {
        self.agent.capabilities()
    }

    fn kind(&self) -> crate::shape::NodeKind {
        crate::shape::NodeKind::ModelStep
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput> {
        use futures::StreamExt;

        // Map state to input content
        let content = (self.input_mapper)(&ctx.state);

        // Create a graph invocation context with the agent
        let invocation_ctx = Arc::new(GraphInvocationContext::with_parent(
            ctx.config.thread_id.clone(),
            content,
            self.agent.clone(),
            ctx.config.parent_context.clone(),
            ctx.run_config(),
            ctx.state.clone(),
        ));

        // Run the agent and collect events
        let branch = adk_core::ReadonlyContext::branch(invocation_ctx.as_ref()).to_string();
        let stream = self.agent.run(invocation_ctx).await.map_err(|e| {
            crate::error::GraphError::AgentFailed { node: self.name.clone(), source: Box::new(e) }
        })?;

        // The first error the agent yields fails the node, as on the streaming
        // path, so a model failure is not a successful node with no answer: a
        // retry policy applies, and the nodes after it do not run.
        let mut stream = stream;
        let mut events: Vec<adk_core::Event> = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(event) => {
                    forward_agent_event(&ctx.config, &branch, &event);
                    events.push(event);
                }
                Err(e) => {
                    return Err(crate::error::GraphError::AgentFailed {
                        node: self.name.clone(),
                        source: Box::new(e),
                    });
                }
            }
        }

        if let Some(request) =
            events.iter().find_map(|event| event.actions.tool_confirmation.clone())
        {
            return Ok(NodeOutput::new().with_interrupt(
                crate::interrupt::GraphToolConfirmationPause::pending_interrupt(
                    self.name.clone(),
                    request,
                ),
            ));
        }

        // Map events to state updates
        let updates = (self.output_mapper)(&events);
        let goto = self.goto_mapper.as_ref().and_then(|mapper| mapper(&updates));

        // Convert agent events to stream events for tracing
        let mut output = NodeOutput::new().with_updates(updates);
        if let Some(targets) = goto {
            output = output.with_goto(targets);
        }
        for event in &events {
            if let Ok(json) = serde_json::to_value(event) {
                output = output.with_event(StreamEvent::custom(&self.name, "agent_event", json));
            }
        }

        Ok(output)
    }

    fn execute_stream<'a>(
        &'a self,
        ctx: &'a NodeContext,
    ) -> Pin<Box<dyn futures::Stream<Item = Result<StreamEvent>> + Send + 'a>> {
        use futures::StreamExt;
        let name = self.name.clone();
        let agent = self.agent.clone();
        let input_mapper = &self.input_mapper;
        let output_mapper = &self.output_mapper;
        let goto_mapper = &self.goto_mapper;
        let parent_context = ctx.config.parent_context.clone();
        let thread_id = ctx.config.thread_id.clone();
        let content = (input_mapper)(&ctx.state);

        Box::pin(async_stream::stream! {
            tracing::debug!("AgentNode::execute_stream called for {}", name);
            let invocation_ctx = Arc::new(GraphInvocationContext::with_parent(
                thread_id,
                content,
                agent.clone(),
                parent_context,
                ctx.run_config(),
                ctx.state.clone(),
            ));

            let branch = adk_core::ReadonlyContext::branch(invocation_ctx.as_ref()).to_string();
            let stream = match agent.run(invocation_ctx).await {
                Ok(s) => s,
                Err(e) => {
                    yield Err(crate::error::GraphError::AgentFailed {
                        node: name.clone(),
                        source: Box::new(e),
                    });
                    return;
                }
            };

            tokio::pin!(stream);
            let mut all_events = Vec::new();
            let mut text_deltas = adk_core::EventTextDeltas::default();

            while let Some(result) = stream.next().await {
                match result {
                    Ok(event) => {
                        forward_agent_event(&ctx.config, &branch, &event);
                        if let Some(request) = event.actions.tool_confirmation.clone() {
                            yield Ok(StreamEvent::node_interrupt(
                                &name,
                                crate::interrupt::GraphToolConfirmationPause::KIND,
                                Some(crate::interrupt::GraphToolConfirmationPause::pending_data(
                                    name.clone(),
                                    request,
                                )),
                            ));
                            return;
                        }
                        // Emit streaming event immediately
                        if let Some(content) = text_deltas.push(&event).content() {
                            let text: String = content.parts.iter().filter_map(|p| p.text()).collect();
                            if !text.is_empty() {
                                yield Ok(StreamEvent::Message {
                                    node: name.clone(),
                                    content: text,
                                    is_final: false,
                                });
                            }
                        }
                        all_events.push(event);
                    }
                    Err(e) => {
                        yield Err(crate::error::GraphError::AgentFailed {
                            node: name.clone(),
                            source: Box::new(e),
                        });
                        return;
                    }
                }
            }

            // Emit final events
            for event in &all_events {
                if let Ok(json) = serde_json::to_value(event) {
                    yield Ok(StreamEvent::custom(&name, "agent_event", json));
                }
            }

            // Report state updates from this run. Without this the streaming
            // executor has no updates to apply and would have to run the agent
            // a second time to obtain them.
            let updates = (output_mapper)(&all_events);
            // Same route the plain path uses: this yields events, not a NodeOutput.
            if let Some(targets) = goto_mapper.as_ref().and_then(|mapper| mapper(&updates)) {
                yield Ok(StreamEvent::route_dispatched(&name, targets));
            }
            yield Ok(StreamEvent::Updates { node: name.clone(), updates });
        })
    }
}

/// Sends an agent node's event to the run's event sink, when it has one, on the
/// node's `branch` unless the agent set one.
///
/// A confirmation request is not forwarded: the graph reports the pause itself,
/// with the checkpoint to resume from, so the caller sees the request once. A
/// transfer the agent asks for names an agent of the node's own tree, not the
/// caller's, so the forwarded event does not carry it.
fn forward_agent_event(config: &ExecutionConfig, branch: &str, event: &adk_core::Event) {
    if let Some(sink) = &config.event_sink
        && event.actions.tool_confirmation.is_none()
    {
        let mut event = event.clone();
        if event.branch.is_empty() {
            event.branch = branch.to_string();
        }
        event.actions.transfer_to_agent = None;
        // The receiver is gone only once the run has finished reporting.
        let _ = sink.send(event);
    }
}

/// Full InvocationContext implementation for running agents within graph nodes
struct GraphInvocationContext {
    invocation_id: String,
    user_content: adk_core::Content,
    agent: Arc<dyn adk_core::Agent>,
    session: Arc<GraphSession>,
    run_config: adk_core::RunConfig,
    ended: std::sync::atomic::AtomicBool,
    /// The invocation this graph run belongs to, when it has one.
    ///
    /// Present: identity, services, request context, and cancellation come from the
    /// caller, so an agent behaves the same inside a graph as outside it.
    /// Absent: standalone mode, with the synthetic identity below.
    parent: Option<Arc<dyn adk_core::InvocationContext>>,
    /// Identity strings, owned because the trait returns them by reference.
    user_id: String,
    app_name: String,
    branch: String,
}

/// Identity used when a graph runs with no parent invocation.
pub(crate) const STANDALONE_USER_ID: &str = "graph_user";
/// Application name used when a graph runs with no parent invocation.
pub(crate) const STANDALONE_APP_NAME: &str = "graph_app";

impl GraphInvocationContext {
    /// `thread_id` identifies the session only in standalone mode; with a parent,
    /// the agent sees the parent's session, invocation, and identity.
    fn with_parent(
        thread_id: String,
        user_content: adk_core::Content,
        agent: Arc<dyn adk_core::Agent>,
        parent: Option<Arc<dyn adk_core::InvocationContext>>,
        explicit_run_config: Option<adk_core::RunConfig>,
        graph_state: State,
    ) -> Self {
        // The node's events belong to the caller's invocation, as a workflow
        // sub-agent's do, so a runner records them under it.
        let invocation_id = parent.as_ref().map_or_else(
            || uuid::Uuid::new_v4().to_string(),
            |parent| parent.invocation_id().to_string(),
        );
        let session = Arc::new(GraphSession::new(thread_id, parent.clone(), graph_state));
        // Add user content to history
        session.append_content(user_content.clone());

        // A node runs on its own branch below the caller's, so events it produces are
        // attributable and do not read as the parent agent's own turn.
        let (user_id, app_name, branch, inherited_run_config) = match parent.as_ref() {
            Some(parent) => (
                parent.user_id().to_string(),
                parent.app_name().to_string(),
                match parent.branch() {
                    "" => agent.name().to_string(),
                    existing => format!("{existing}.{}", agent.name()),
                },
                parent.run_config().clone(),
            ),
            None => (
                STANDALONE_USER_ID.to_string(),
                STANDALONE_APP_NAME.to_string(),
                "main".to_string(),
                adk_core::RunConfig::default(),
            ),
        };

        Self {
            invocation_id,
            user_content,
            agent,
            session,
            run_config: explicit_run_config.unwrap_or(inherited_run_config),
            ended: std::sync::atomic::AtomicBool::new(false),
            parent,
            user_id,
            app_name,
            branch,
        }
    }
}

// Implement ReadonlyContext (required by CallbackContext)
impl adk_core::ReadonlyContext for GraphInvocationContext {
    fn invocation_id(&self) -> &str {
        &self.invocation_id
    }

    fn agent_name(&self) -> &str {
        self.agent.name()
    }

    fn user_id(&self) -> &str {
        &self.user_id
    }

    fn app_name(&self) -> &str {
        &self.app_name
    }

    fn session_id(&self) -> &str {
        adk_core::Session::id(self.session.as_ref())
    }

    fn branch(&self) -> &str {
        &self.branch
    }

    fn user_content(&self) -> &adk_core::Content {
        &self.user_content
    }

    fn acting_context(&self) -> adk_core::ActingContext {
        self.run_config.acting_context_for(&self.app_name, self.agent.name(), &self.user_id)
    }

    fn is_authenticated(&self) -> bool {
        self.parent.as_ref().is_some_and(|parent| parent.is_authenticated())
    }
}

// Implement CallbackContext (required by InvocationContext)
#[async_trait]
impl adk_core::CallbackContext for GraphInvocationContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        self.parent.as_ref().and_then(|parent| parent.artifacts())
    }

    fn shared_state(&self) -> Option<Arc<adk_core::SharedState>> {
        self.parent.as_ref().and_then(|parent| parent.shared_state())
    }
}

// Implement InvocationContext
#[async_trait]
impl adk_core::InvocationContext for GraphInvocationContext {
    fn agent(&self) -> Arc<dyn adk_core::Agent> {
        self.agent.clone()
    }

    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        self.parent.as_ref().and_then(|parent| parent.memory())
    }

    fn session(&self) -> &dyn adk_core::Session {
        self.session.as_ref()
    }

    fn run_config(&self) -> &adk_core::RunConfig {
        &self.run_config
    }

    fn end_invocation(&self) {
        self.ended.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(parent) = &self.parent {
            parent.end_invocation();
        }
    }

    fn ended(&self) -> bool {
        self.ended.load(std::sync::atomic::Ordering::SeqCst)
            || self.parent.as_ref().is_some_and(|parent| parent.ended())
    }

    fn is_cancelled(&self) -> bool {
        self.parent.as_ref().is_some_and(|parent| parent.is_cancelled())
    }

    fn user_scopes(&self) -> Vec<String> {
        self.parent.as_ref().map(|parent| parent.user_scopes()).unwrap_or_default()
    }

    fn request_metadata(&self) -> std::collections::HashMap<String, Value> {
        self.parent.as_ref().map(|parent| parent.request_metadata()).unwrap_or_default()
    }

    async fn get_secret(&self, name: &str) -> adk_core::Result<Option<String>> {
        match &self.parent {
            Some(parent) => parent.get_secret(name).await,
            None => Ok(None),
        }
    }

    async fn get_secret_for(
        &self,
        request: &adk_core::SecretRequest,
    ) -> adk_core::Result<Option<String>> {
        match &self.parent {
            Some(parent) => parent.get_secret_for(request).await,
            None => Ok(None),
        }
    }
}

/// The session an agent node's agent sees.
///
/// With a parent invocation it is a view of the caller's session: the same id,
/// app, and user, and the session's state, read live, with the graph's state
/// layered on top, so an instruction template resolves both. Its conversation is
/// the node's own: the input the node's mapper built and what the agent adds.
/// Writes through [`adk_core::State::set`] stay in the view. Standalone, the
/// session is the graph's thread.
struct GraphSession {
    /// The thread id, the session id when there is no parent.
    thread_id: String,
    parent: Option<Arc<dyn adk_core::InvocationContext>>,
    state: GraphState,
    history: std::sync::RwLock<Vec<adk_core::Content>>,
}

impl GraphSession {
    fn new(
        thread_id: String,
        parent: Option<Arc<dyn adk_core::InvocationContext>>,
        graph_state: State,
    ) -> Self {
        Self {
            thread_id,
            state: GraphState::new(parent.clone(), graph_state),
            parent,
            history: std::sync::RwLock::new(Vec::new()),
        }
    }

    fn append_content(&self, content: adk_core::Content) {
        if let Ok(mut h) = self.history.write() {
            h.push(content);
        }
    }
}

impl adk_core::Session for GraphSession {
    fn id(&self) -> &str {
        self.parent.as_ref().map_or(self.thread_id.as_str(), |parent| parent.session().id())
    }

    fn app_name(&self) -> &str {
        self.parent.as_ref().map_or(STANDALONE_APP_NAME, |parent| parent.session().app_name())
    }

    fn user_id(&self) -> &str {
        self.parent.as_ref().map_or(STANDALONE_USER_ID, |parent| parent.session().user_id())
    }

    fn state(&self) -> &dyn adk_core::State {
        &self.state
    }

    fn conversation_history(&self) -> Vec<adk_core::Content> {
        self.history.read().ok().map(|h| h.clone()).unwrap_or_default()
    }

    fn append_to_history(&self, content: adk_core::Content) {
        self.append_content(content);
    }
}

/// Session state for an agent node: its own writes, over the graph's state, over
/// the caller's session state.
struct GraphState {
    parent: Option<Arc<dyn adk_core::InvocationContext>>,
    graph: State,
    data: std::sync::RwLock<std::collections::HashMap<String, serde_json::Value>>,
}

impl GraphState {
    fn new(parent: Option<Arc<dyn adk_core::InvocationContext>>, graph: State) -> Self {
        Self { parent, graph, data: std::sync::RwLock::new(std::collections::HashMap::new()) }
    }
}

impl adk_core::State for GraphState {
    fn get(&self, key: &str) -> Option<serde_json::Value> {
        if let Some(value) = self.data.read().ok().and_then(|data| data.get(key).cloned()) {
            return Some(value);
        }
        if let Some(value) = self.graph.get(key) {
            return Some(value.clone());
        }
        self.parent.as_ref().and_then(|parent| parent.session().state().get(key))
    }

    fn set(&mut self, key: String, value: serde_json::Value) {
        if let Err(msg) = adk_core::validate_state_key(&key) {
            tracing::warn!(key = %key, "rejecting invalid state key: {msg}");
            return;
        }
        if let Ok(mut data) = self.data.write() {
            data.insert(key, value);
        }
    }

    fn all(&self) -> std::collections::HashMap<String, serde_json::Value> {
        let mut all =
            self.parent.as_ref().map(|parent| parent.session().state().all()).unwrap_or_default();
        all.extend(self.graph.iter().map(|(key, value)| (key.clone(), value.clone())));
        if let Ok(data) = self.data.read() {
            all.extend(data.iter().map(|(key, value)| (key.clone(), value.clone())));
        }
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_function_node() {
        let node = FunctionNode::new("test", |_ctx| async {
            Ok(NodeOutput::new().with_update("result", serde_json::json!("success")))
        });

        assert_eq!(node.name(), "test");

        let ctx = NodeContext::new(State::new(), ExecutionConfig::default(), 0);
        let output = node.execute(&ctx).await.unwrap();

        assert_eq!(output.updates.get("result"), Some(&serde_json::json!("success")));
    }

    #[tokio::test]
    async fn test_passthrough_node() {
        let node = PassthroughNode::new("pass");
        let ctx = NodeContext::new(State::new(), ExecutionConfig::default(), 0);
        let output = node.execute(&ctx).await.unwrap();

        assert!(output.updates.is_empty());
        assert!(output.interrupt.is_none());
    }

    #[test]
    fn default_output_mapper_does_not_repeat_a_complete_snapshot() {
        let mut partial = adk_core::Event::new("invocation");
        partial.llm_response.partial = true;
        partial.set_content(adk_core::Content::new("model").with_text("Hello"));
        let mut terminal = partial.clone();
        terminal.llm_response.partial = false;
        terminal.llm_response.provider_metadata =
            Some(serde_json::json!({"content_complete": true}));
        terminal.set_content(adk_core::Content::new("model").with_text("Hello world"));

        let updates = default_output_mapper(&[partial, terminal]);

        assert_eq!(
            updates.get("messages"),
            Some(&serde_json::json!([
                {"role": "assistant", "content": "Hello"},
                {"role": "assistant", "content": " world"},
            ]))
        );
    }

    #[test]
    fn test_node_output_builder() {
        let output = NodeOutput::new().with_update("a", 1).with_update("b", "hello");

        assert_eq!(output.updates.get("a"), Some(&serde_json::json!(1)));
        assert_eq!(output.updates.get("b"), Some(&serde_json::json!("hello")));
    }
}
