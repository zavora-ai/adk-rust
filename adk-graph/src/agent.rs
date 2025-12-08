//! GraphAgent - ADK Agent integration for graph workflows
//!
//! Provides a builder pattern similar to LlmAgent and RealtimeAgent.

use crate::checkpoint::Checkpointer;
use crate::edge::{Edge, EdgeTarget, END, START};
use crate::error::{GraphError, Result};
use crate::graph::{CompiledGraph, StateGraph};
use crate::node::{ExecutionConfig, FunctionNode, Node, NodeContext, NodeOutput};
use crate::state::{State, StateSchema};
use crate::stream::{StreamEvent, StreamMode};
use adk_core::{Agent, Content, Event, EventStream, InvocationContext};
use async_trait::async_trait;
use serde_json::json;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Type alias for callbacks
pub type BeforeAgentCallback = Arc<
    dyn Fn(Arc<dyn InvocationContext>) -> Pin<Box<dyn Future<Output = adk_core::Result<()>> + Send>>
        + Send
        + Sync,
>;

pub type AfterAgentCallback = Arc<
    dyn Fn(
            Arc<dyn InvocationContext>,
            &Event,
        ) -> Pin<Box<dyn Future<Output = adk_core::Result<()>> + Send>>
        + Send
        + Sync,
>;

/// Type alias for input mapper function
pub type InputMapper = Arc<dyn Fn(&dyn InvocationContext) -> State + Send + Sync>;

/// Type alias for output mapper function
pub type OutputMapper = Arc<dyn Fn(&State) -> Vec<Event> + Send + Sync>;

/// GraphAgent wraps a CompiledGraph as an ADK Agent
pub struct GraphAgent {
    name: String,
    description: String,
    graph: Arc<CompiledGraph>,
    /// Map InvocationContext to graph input state
    input_mapper: InputMapper,
    /// Map graph output state to ADK Events
    output_mapper: OutputMapper,
    /// Before agent callback
    before_callback: Option<BeforeAgentCallback>,
    /// After agent callback
    after_callback: Option<AfterAgentCallback>,
}

impl GraphAgent {
    /// Create a new GraphAgent builder
    pub fn builder(name: &str) -> GraphAgentBuilder {
        GraphAgentBuilder::new(name)
    }

    /// Create directly from a compiled graph
    pub fn from_graph(name: &str, graph: CompiledGraph) -> Self {
        Self {
            name: name.to_string(),
            description: String::new(),
            graph: Arc::new(graph),
            input_mapper: Arc::new(default_input_mapper),
            output_mapper: Arc::new(default_output_mapper),
            before_callback: None,
            after_callback: None,
        }
    }

    /// Get the underlying compiled graph
    pub fn graph(&self) -> &CompiledGraph {
        &self.graph
    }

    /// Execute the graph directly (bypassing Agent trait)
    pub async fn invoke(&self, input: State, config: ExecutionConfig) -> Result<State> {
        self.graph.invoke(input, config).await
    }

    /// Stream execution
    pub fn stream(
        &self,
        input: State,
        config: ExecutionConfig,
        mode: StreamMode,
    ) -> impl futures::Stream<Item = Result<StreamEvent>> + '_ {
        self.graph.stream(input, config, mode)
    }
}

#[async_trait]
impl Agent for GraphAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> adk_core::Result<EventStream> {
        // Call before callback
        if let Some(callback) = &self.before_callback {
            callback(ctx.clone()).await?;
        }

        // Map context to input state
        let input = (self.input_mapper)(ctx.as_ref());

        // Create execution config from context
        let config = ExecutionConfig::new(ctx.session_id());

        // Execute graph
        let graph = self.graph.clone();
        let output_mapper = self.output_mapper.clone();
        let after_callback = self.after_callback.clone();
        let ctx_clone = ctx.clone();

        let stream = async_stream::stream! {
            match graph.invoke(input, config).await {
                Ok(state) => {
                    let events = output_mapper(&state);
                    for event in events {
                        // Call after callback for each event
                        if let Some(callback) = &after_callback {
                            if let Err(e) = callback(ctx_clone.clone(), &event).await {
                                yield Err(e);
                                return;
                            }
                        }
                        yield Ok(event);
                    }
                }
                Err(GraphError::Interrupted(interrupt)) => {
                    // Create an interrupt event
                    let mut event = Event::new("graph_interrupted");
                    event.set_content(Content::new("assistant").with_text(format!(
                        "Graph interrupted: {:?}\nThread: {}\nCheckpoint: {}",
                        interrupt.interrupt,
                        interrupt.thread_id,
                        interrupt.checkpoint_id
                    )));
                    yield Ok(event);
                }
                Err(e) => {
                    yield Err(adk_core::AdkError::Agent(e.to_string()));
                }
            }
        };

        Ok(Box::pin(stream))
    }
}

/// Default input mapper - extracts content from InvocationContext
fn default_input_mapper(ctx: &dyn InvocationContext) -> State {
    let mut state = State::new();

    // Get user content
    let content = ctx.user_content();
    let text: String = content.parts.iter().filter_map(|p| p.text()).collect::<Vec<_>>().join("\n");

    if !text.is_empty() {
        state.insert("input".to_string(), json!(text));
        state.insert("messages".to_string(), json!([{"role": "user", "content": text}]));
    }

    // Add session ID
    state.insert("session_id".to_string(), json!(ctx.session_id()));

    state
}

/// Default output mapper - creates events from state
fn default_output_mapper(state: &State) -> Vec<Event> {
    let mut events = Vec::new();

    // Try to get output from common fields
    let output_text = state
        .get("output")
        .and_then(|v| v.as_str())
        .or_else(|| state.get("result").and_then(|v| v.as_str()))
        .or_else(|| {
            state
                .get("messages")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.last())
                .and_then(|msg| msg.get("content"))
                .and_then(|c| c.as_str())
        });

    let text = if let Some(text) = output_text {
        text.to_string()
    } else {
        // Return the full state as JSON
        serde_json::to_string_pretty(state).unwrap_or_default()
    };

    let mut event = Event::new("graph_output");
    event.set_content(Content::new("assistant").with_text(&text));
    events.push(event);

    events
}

/// Builder for GraphAgent
pub struct GraphAgentBuilder {
    name: String,
    description: String,
    schema: StateSchema,
    nodes: Vec<Arc<dyn Node>>,
    edges: Vec<Edge>,
    checkpointer: Option<Arc<dyn Checkpointer>>,
    interrupt_before: Vec<String>,
    interrupt_after: Vec<String>,
    recursion_limit: usize,
    input_mapper: Option<InputMapper>,
    output_mapper: Option<OutputMapper>,
    before_callback: Option<BeforeAgentCallback>,
    after_callback: Option<AfterAgentCallback>,
}

impl GraphAgentBuilder {
    /// Create a new builder
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            description: String::new(),
            schema: StateSchema::simple(&["input", "output", "messages"]),
            nodes: vec![],
            edges: vec![],
            checkpointer: None,
            interrupt_before: vec![],
            interrupt_after: vec![],
            recursion_limit: 50,
            input_mapper: None,
            output_mapper: None,
            before_callback: None,
            after_callback: None,
        }
    }

    /// Set description
    pub fn description(mut self, desc: &str) -> Self {
        self.description = desc.to_string();
        self
    }

    /// Set state schema
    pub fn state_schema(mut self, schema: StateSchema) -> Self {
        self.schema = schema;
        self
    }

    /// Add channels to state schema
    pub fn channels(mut self, channels: &[&str]) -> Self {
        self.schema = StateSchema::simple(channels);
        self
    }

    /// Add a node
    pub fn node<N: Node + 'static>(mut self, node: N) -> Self {
        self.nodes.push(Arc::new(node));
        self
    }

    /// Add a function as a node
    pub fn node_fn<F, Fut>(mut self, name: &str, func: F) -> Self
    where
        F: Fn(NodeContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<NodeOutput>> + Send + 'static,
    {
        self.nodes.push(Arc::new(FunctionNode::new(name, func)));
        self
    }

    /// Add a direct edge
    pub fn edge(mut self, source: &str, target: &str) -> Self {
        let target =
            if target == END { EdgeTarget::End } else { EdgeTarget::Node(target.to_string()) };

        if source == START {
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

    /// Add a conditional edge
    pub fn conditional_edge<F, I>(mut self, source: &str, router: F, targets: I) -> Self
    where
        F: Fn(&State) -> String + Send + Sync + 'static,
        I: IntoIterator<Item = (&'static str, &'static str)>,
    {
        let targets_map: HashMap<String, EdgeTarget> = targets
            .into_iter()
            .map(|(k, v)| {
                let target =
                    if v == END { EdgeTarget::End } else { EdgeTarget::Node(v.to_string()) };
                (k.to_string(), target)
            })
            .collect();

        self.edges.push(Edge::Conditional {
            source: source.to_string(),
            router: Arc::new(router),
            targets: targets_map,
        });

        self
    }

    /// Set checkpointer
    pub fn checkpointer<C: Checkpointer + 'static>(mut self, checkpointer: C) -> Self {
        self.checkpointer = Some(Arc::new(checkpointer));
        self
    }

    /// Set checkpointer with Arc
    pub fn checkpointer_arc(mut self, checkpointer: Arc<dyn Checkpointer>) -> Self {
        self.checkpointer = Some(checkpointer);
        self
    }

    /// Set nodes to interrupt before
    pub fn interrupt_before(mut self, nodes: &[&str]) -> Self {
        self.interrupt_before = nodes.iter().map(|s| s.to_string()).collect();
        self
    }

    /// Set nodes to interrupt after
    pub fn interrupt_after(mut self, nodes: &[&str]) -> Self {
        self.interrupt_after = nodes.iter().map(|s| s.to_string()).collect();
        self
    }

    /// Set recursion limit
    pub fn recursion_limit(mut self, limit: usize) -> Self {
        self.recursion_limit = limit;
        self
    }

    /// Set custom input mapper
    pub fn input_mapper<F>(mut self, mapper: F) -> Self
    where
        F: Fn(&dyn InvocationContext) -> State + Send + Sync + 'static,
    {
        self.input_mapper = Some(Arc::new(mapper));
        self
    }

    /// Set custom output mapper
    pub fn output_mapper<F>(mut self, mapper: F) -> Self
    where
        F: Fn(&State) -> Vec<Event> + Send + Sync + 'static,
    {
        self.output_mapper = Some(Arc::new(mapper));
        self
    }

    /// Set before agent callback
    pub fn before_agent_callback<F, Fut>(mut self, callback: F) -> Self
    where
        F: Fn(Arc<dyn InvocationContext>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = adk_core::Result<()>> + Send + 'static,
    {
        self.before_callback = Some(Arc::new(move |ctx| Box::pin(callback(ctx))));
        self
    }

    /// Set after agent callback
    pub fn after_agent_callback<F, Fut>(mut self, _callback: F) -> Self
    where
        F: Fn(Arc<dyn InvocationContext>, &Event) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = adk_core::Result<()>> + Send + 'static,
    {
        // Note: Full callback implementation requires more complex lifetime handling.
        // For now, this is a placeholder that accepts the callback but doesn't store it.
        // TODO: Implement proper after_agent_callback with event cloning
        self.after_callback = Some(Arc::new(move |_ctx, _event| Box::pin(async move { Ok(()) })));
        self
    }

    /// Build the GraphAgent
    pub fn build(self) -> Result<GraphAgent> {
        // Build the graph
        let mut graph = StateGraph::new(self.schema);

        // Add nodes
        for node in self.nodes {
            graph.nodes.insert(node.name().to_string(), node);
        }

        // Add edges
        graph.edges = self.edges;

        // Compile
        let mut compiled = graph.compile()?;

        // Configure
        if let Some(cp) = self.checkpointer {
            compiled.checkpointer = Some(cp);
        }
        compiled.interrupt_before = self.interrupt_before.into_iter().collect();
        compiled.interrupt_after = self.interrupt_after.into_iter().collect();
        compiled.recursion_limit = self.recursion_limit;

        Ok(GraphAgent {
            name: self.name,
            description: self.description,
            graph: Arc::new(compiled),
            input_mapper: self.input_mapper.unwrap_or(Arc::new(default_input_mapper)),
            output_mapper: self.output_mapper.unwrap_or(Arc::new(default_output_mapper)),
            before_callback: self.before_callback,
            after_callback: self.after_callback,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_graph_agent_builder() {
        let agent = GraphAgent::builder("test")
            .description("Test agent")
            .channels(&["value"])
            .node_fn("set", |_ctx| async { Ok(NodeOutput::new().with_update("value", json!(42))) })
            .edge(START, "set")
            .edge("set", END)
            .build()
            .unwrap();

        assert_eq!(agent.name(), "test");
        assert_eq!(agent.description(), "Test agent");

        // Test direct invocation
        let result = agent.invoke(State::new(), ExecutionConfig::new("test")).await.unwrap();

        assert_eq!(result.get("value"), Some(&json!(42)));
    }
}
