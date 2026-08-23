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
    /// Recursion limit for cycles
    pub recursion_limit: usize,
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
}

impl ExecutionConfig {
    /// Create a new config with the given thread ID
    pub fn new(thread_id: &str) -> Self {
        Self {
            thread_id: thread_id.to_string(),
            resume_from: None,
            recursion_limit: 50,
            metadata: HashMap::new(),
            parent_context: None,
        }
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

    /// Set the recursion limit
    pub fn with_recursion_limit(mut self, limit: usize) -> Self {
        self.recursion_limit = limit;
        self
    }

    /// Resume from a specific checkpoint
    pub fn with_resume_from(mut self, checkpoint_id: &str) -> Self {
        self.resume_from = Some(checkpoint_id.to_string());
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
}

impl NodeContext {
    /// Create a new node context
    pub fn new(state: State, config: ExecutionConfig, step: usize) -> Self {
        Self { state, config, step, progress_handle: None, children: None, parent_schema: None }
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

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput> {
        // The closure takes an owned context, so everything the executor attached
        // has to be carried across or the node silently loses it.
        let mut ctx_owned = NodeContext::new(ctx.state.clone(), ctx.config.clone(), ctx.step);
        if let Some(handle) = ctx.progress_handle() {
            ctx_owned.set_progress_handle(handle.clone());
        }
        if let Some(invoker) = ctx.child_invoker() {
            ctx_owned.set_child_invoker(invoker);
        }
        if let Some(schema) = ctx.parent_schema() {
            ctx_owned.set_parent_schema(schema);
        }
        (self.func)(ctx_owned).await
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
    for event in events {
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
        ));

        // Run the agent and collect events
        let stream = self.agent.run(invocation_ctx).await.map_err(|e| {
            crate::error::GraphError::NodeExecutionFailed {
                node: self.name.clone(),
                message: e.to_string(),
            }
        })?;

        let events: Vec<adk_core::Event> = stream.filter_map(|r| async { r.ok() }).collect().await;

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
            ));

            let stream = match agent.run(invocation_ctx).await {
                Ok(s) => s,
                Err(e) => {
                    yield Err(crate::error::GraphError::NodeExecutionFailed {
                        node: name.clone(),
                        message: e.to_string(),
                    });
                    return;
                }
            };

            tokio::pin!(stream);
            let mut all_events = Vec::new();

            while let Some(result) = stream.next().await {
                match result {
                    Ok(event) => {
                        // Emit streaming event immediately
                        if let Some(content) = event.content() {
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
                        yield Err(crate::error::GraphError::NodeExecutionFailed {
                            node: name.clone(),
                            message: e.to_string(),
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
const STANDALONE_USER_ID: &str = "graph_user";
/// Application name used when a graph runs with no parent invocation.
const STANDALONE_APP_NAME: &str = "graph_app";

impl GraphInvocationContext {
    fn with_parent(
        session_id: String,
        user_content: adk_core::Content,
        agent: Arc<dyn adk_core::Agent>,
        parent: Option<Arc<dyn adk_core::InvocationContext>>,
    ) -> Self {
        let invocation_id = uuid::Uuid::new_v4().to_string();
        let session = Arc::new(GraphSession::new(session_id));
        // Add user content to history
        session.append_content(user_content.clone());

        // A node runs on its own branch below the caller's, so events it produces are
        // attributable and do not read as the parent agent's own turn.
        let (user_id, app_name, branch, run_config) = match parent.as_ref() {
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
            run_config,
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
        &self.session.id
    }

    fn branch(&self) -> &str {
        &self.branch
    }

    fn user_content(&self) -> &adk_core::Content {
        &self.user_content
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

/// Minimal Session implementation for graph execution
struct GraphSession {
    id: String,
    state: GraphState,
    history: std::sync::RwLock<Vec<adk_core::Content>>,
}

impl GraphSession {
    fn new(id: String) -> Self {
        Self { id, state: GraphState::new(), history: std::sync::RwLock::new(Vec::new()) }
    }

    fn append_content(&self, content: adk_core::Content) {
        if let Ok(mut h) = self.history.write() {
            h.push(content);
        }
    }
}

impl adk_core::Session for GraphSession {
    fn id(&self) -> &str {
        &self.id
    }

    fn app_name(&self) -> &str {
        "graph_app"
    }

    fn user_id(&self) -> &str {
        "graph_user"
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

/// Minimal State implementation for graph execution
struct GraphState {
    data: std::sync::RwLock<std::collections::HashMap<String, serde_json::Value>>,
}

impl GraphState {
    fn new() -> Self {
        Self { data: std::sync::RwLock::new(std::collections::HashMap::new()) }
    }
}

impl adk_core::State for GraphState {
    fn get(&self, key: &str) -> Option<serde_json::Value> {
        self.data.read().ok()?.get(key).cloned()
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
        self.data.read().ok().map(|d| d.clone()).unwrap_or_default()
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
    fn test_node_output_builder() {
        let output = NodeOutput::new().with_update("a", 1).with_update("b", "hello");

        assert_eq!(output.updates.get("a"), Some(&serde_json::json!(1)));
        assert_eq!(output.updates.get("b"), Some(&serde_json::json!("hello")));
    }
}
