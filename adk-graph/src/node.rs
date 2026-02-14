//! Node types for graph execution
//!
//! Nodes are the computational units in a graph. They receive state and return updates.

use crate::error::Result;
use crate::interrupt::Interrupt;
use crate::state::State;
use crate::stream::StreamEvent;
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
}

impl ExecutionConfig {
    /// Create a new config with the given thread ID
    pub fn new(thread_id: &str) -> Self {
        Self {
            thread_id: thread_id.to_string(),
            resume_from: None,
            recursion_limit: 50,
            metadata: HashMap::new(),
        }
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
}

impl NodeContext {
    /// Create a new node context
    pub fn new(state: State, config: ExecutionConfig, step: usize) -> Self {
        Self { state, config, step }
    }

    /// Get a value from state
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.state.get(key)
    }

    /// Get a value from state as a specific type
    pub fn get_as<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.state.get(key).and_then(|v| serde_json::from_value(v.clone()).ok())
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
}

impl NodeOutput {
    /// Create a new empty output
    pub fn new() -> Self {
        Self::default()
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

    /// Execute the node and return state updates
    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput>;

    /// Stream execution events (default: wraps execute)
    fn execute_stream<'a>(
        &'a self,
        ctx: &'a NodeContext,
    ) -> Pin<Box<dyn futures::Stream<Item = Result<StreamEvent>> + Send + 'a>> {
        let _name = self.name().to_string();
        Box::pin(async_stream::stream! {
            match self.execute(ctx).await {
                Ok(output) => {
                    for event in output.events {
                        yield Ok(event);
                    }
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
        let ctx_owned =
            NodeContext { state: ctx.state.clone(), config: ctx.config.clone(), step: ctx.step };
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

/// Wrapper to use an existing ADK Agent as a graph node
pub struct AgentNode {
    name: String,
    #[allow(dead_code)]
    agent: Arc<dyn adk_core::Agent>,
    /// Map state to agent input content
    input_mapper: AgentInputMapper,
    /// Map agent events to state updates
    output_mapper: AgentOutputMapper,
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
}

/// Default input mapper - looks for "messages" or "input" in state
fn default_input_mapper(state: &State) -> adk_core::Content {
    // Try to get messages first
    if let Some(messages) = state.get("messages") {
        if let Some(arr) = messages.as_array() {
            if let Some(last) = arr.last() {
                if let Some(content) = last.get("content").and_then(|c| c.as_str()) {
                    return adk_core::Content::new("user").with_text(content);
                }
            }
        }
    }

    // Try input field
    if let Some(input) = state.get("input") {
        if let Some(text) = input.as_str() {
            return adk_core::Content::new("user").with_text(text);
        }
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

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput> {
        use futures::StreamExt;

        // Map state to input content
        let content = (self.input_mapper)(&ctx.state);

        // Create a graph invocation context with the agent
        let invocation_ctx = Arc::new(GraphInvocationContext::new(
            ctx.config.thread_id.clone(),
            content,
            self.agent.clone(),
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

        // Convert agent events to stream events for tracing
        let mut output = NodeOutput::new().with_updates(updates);
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
        let thread_id = ctx.config.thread_id.clone();
        let content = (input_mapper)(&ctx.state);

        Box::pin(async_stream::stream! {
            tracing::debug!("AgentNode::execute_stream called for {}", name);
            let invocation_ctx = Arc::new(GraphInvocationContext::new(
                thread_id,
                content,
                agent.clone(),
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
}

impl GraphInvocationContext {
    fn new(
        session_id: String,
        user_content: adk_core::Content,
        agent: Arc<dyn adk_core::Agent>,
    ) -> Self {
        let invocation_id = uuid::Uuid::new_v4().to_string();
        let session = Arc::new(GraphSession::new(session_id));
        // Add user content to history
        session.append_content(user_content.clone());
        Self {
            invocation_id,
            user_content,
            agent,
            session,
            run_config: adk_core::RunConfig::default(),
            ended: std::sync::atomic::AtomicBool::new(false),
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
        "graph_user"
    }

    fn app_name(&self) -> &str {
        "graph_app"
    }

    fn session_id(&self) -> &str {
        &self.session.id
    }

    fn branch(&self) -> &str {
        "main"
    }

    fn user_content(&self) -> &adk_core::Content {
        &self.user_content
    }
}

// Implement CallbackContext (required by InvocationContext)
#[async_trait]
impl adk_core::CallbackContext for GraphInvocationContext {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

// Implement InvocationContext
#[async_trait]
impl adk_core::InvocationContext for GraphInvocationContext {
    fn agent(&self) -> Arc<dyn adk_core::Agent> {
        self.agent.clone()
    }

    fn memory(&self) -> Option<Arc<dyn adk_core::Memory>> {
        None
    }

    fn session(&self) -> &dyn adk_core::Session {
        self.session.as_ref()
    }

    fn run_config(&self) -> &adk_core::RunConfig {
        &self.run_config
    }

    fn end_invocation(&self) {
        self.ended.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    fn ended(&self) -> bool {
        self.ended.load(std::sync::atomic::Ordering::SeqCst)
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
