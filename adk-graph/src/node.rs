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
        // Map state to input content
        let _content = (self.input_mapper)(&ctx.state);

        // Create a minimal invocation context
        // Note: In full implementation, this would use adk-runner's InvocationContext
        // For now, we'll use a simplified approach

        // Run the agent and collect events
        // This is a simplified version - full implementation would integrate with adk-runner
        let events: Vec<adk_core::Event> = Vec::new();

        // TODO: Actually run the agent with proper context
        // let stream = self.agent.run(invocation_ctx).await?;
        // let events: Vec<_> = stream.collect().await;

        // Map events to state updates
        let updates = (self.output_mapper)(&events);

        Ok(NodeOutput::new().with_updates(updates))
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
