# ADK-Rust GraphAgent Design

## Overview

This document describes the design for `adk-graph`, a new crate that brings LangGraph-style graph-based workflow orchestration to ADK-Rust. The design retains ADK's core philosophy of type safety, streaming, and composability while providing the flexibility and power of graph-based agent workflows.

## Design Goals

1. **LangGraph Feature Parity**: Support cycles, conditional routing, parallel execution, checkpointing, and human-in-the-loop
2. **ADK Integration**: Implement the `Agent` trait so GraphAgents work seamlessly with existing infrastructure
3. **Type Safety**: Leverage Rust's type system for compile-time guarantees where possible
4. **Streaming First**: Native support for ADK's EventStream pattern
5. **Composability**: Graphs as nodes, existing agents as nodes, subgraph support
6. **Production Ready**: Checkpointing, persistence, fault tolerance

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│                            adk-graph                                     │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐              │
│  │ StateGraph   │───▶│CompiledGraph │───▶│ GraphAgent   │              │
│  │  (Builder)   │    │  (Executor)  │    │(Agent trait) │              │
│  └──────────────┘    └──────────────┘    └──────────────┘              │
│         │                   │                   │                       │
│         ▼                   ▼                   ▼                       │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐              │
│  │    Nodes     │    │   Pregel     │    │  EventStream │              │
│  │   Edges      │    │   Engine     │    │   Mapping    │              │
│  │  Reducers    │    │              │    │              │              │
│  └──────────────┘    └──────────────┘    └──────────────┘              │
│                             │                                           │
│                             ▼                                           │
│                      ┌──────────────┐                                  │
│                      │Checkpointer  │                                  │
│                      │  (Optional)  │                                  │
│                      └──────────────┘                                  │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘
```

## Core Types

### 1. State System

```rust
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Dynamic state using JSON for maximum flexibility
pub type State = HashMap<String, serde_json::Value>;

/// Typed state wrapper for type-safe access
pub struct TypedState<T> {
    inner: State,
    _marker: PhantomData<T>,
}

/// Reducer determines how state updates are merged
#[derive(Clone)]
pub enum Reducer {
    /// Replace the value entirely (default)
    Overwrite,
    /// Append to a list
    Append,
    /// Sum numeric values
    Sum,
    /// Custom merge function
    Custom(Arc<dyn Fn(Value, Value) -> Value + Send + Sync>),
}

/// Channel definition for a state field
pub struct Channel {
    pub name: String,
    pub reducer: Reducer,
    pub default: Option<Value>,
}

/// State schema defines channels and their reducers
pub struct StateSchema {
    pub channels: HashMap<String, Channel>,
}

impl StateSchema {
    pub fn builder() -> StateSchemaBuilder {
        StateSchemaBuilder::default()
    }
}

pub struct StateSchemaBuilder {
    channels: HashMap<String, Channel>,
}

impl StateSchemaBuilder {
    /// Add a channel with overwrite semantics
    pub fn channel(mut self, name: &str) -> Self {
        self.channels.insert(name.to_string(), Channel {
            name: name.to_string(),
            reducer: Reducer::Overwrite,
            default: None,
        });
        self
    }

    /// Add a channel with append semantics (for lists)
    pub fn list_channel(mut self, name: &str) -> Self {
        self.channels.insert(name.to_string(), Channel {
            name: name.to_string(),
            reducer: Reducer::Append,
            default: Some(json!([])),
        });
        self
    }

    /// Add a channel with custom reducer
    pub fn channel_with_reducer(mut self, name: &str, reducer: Reducer) -> Self {
        self.channels.insert(name.to_string(), Channel {
            name: name.to_string(),
            reducer,
            default: None,
        });
        self
    }

    pub fn build(self) -> StateSchema {
        StateSchema { channels: self.channels }
    }
}
```

### 2. Node System

```rust
use async_trait::async_trait;

/// Output from a node execution
pub struct NodeOutput {
    /// State updates to apply
    pub updates: HashMap<String, Value>,
    /// Optional interrupt request
    pub interrupt: Option<Interrupt>,
    /// Custom stream events
    pub events: Vec<StreamEvent>,
}

impl NodeOutput {
    pub fn new() -> Self {
        Self {
            updates: HashMap::new(),
            interrupt: None,
            events: vec![],
        }
    }

    pub fn with_update(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.updates.insert(key.to_string(), value.into());
        self
    }

    pub fn with_interrupt(mut self, interrupt: Interrupt) -> Self {
        self.interrupt = Some(interrupt);
        self
    }
}

/// Context passed to nodes during execution
pub struct NodeContext {
    /// Current graph state (read-only view)
    pub state: State,
    /// Configuration for this execution
    pub config: ExecutionConfig,
    /// Access to checkpointer for manual saves
    pub checkpointer: Option<Arc<dyn Checkpointer>>,
    /// Access to long-term memory store
    pub store: Option<Arc<dyn MemoryStore>>,
}

/// A node in the graph
#[async_trait]
pub trait Node: Send + Sync {
    /// Node identifier
    fn name(&self) -> &str;

    /// Execute the node and return state updates
    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput>;

    /// Optional: Streaming execution for real-time updates
    fn execute_stream(&self, ctx: &NodeContext) -> Option<NodeOutputStream> {
        None // Default: no streaming
    }
}

/// Function node - wraps an async function as a node
pub struct FunctionNode<F> {
    name: String,
    func: F,
}

impl<F, Fut> FunctionNode<F>
where
    F: Fn(&NodeContext) -> Fut + Send + Sync,
    Fut: Future<Output = Result<NodeOutput>> + Send,
{
    pub fn new(name: &str, func: F) -> Self {
        Self {
            name: name.to_string(),
            func,
        }
    }
}

/// Agent node - wraps an existing ADK Agent as a graph node
pub struct AgentNode {
    name: String,
    agent: Arc<dyn Agent>,
    /// Map state to agent input
    input_mapper: Box<dyn Fn(&State) -> Content + Send + Sync>,
    /// Map agent output to state updates
    output_mapper: Box<dyn Fn(&Event) -> HashMap<String, Value> + Send + Sync>,
}

impl AgentNode {
    pub fn new(agent: Arc<dyn Agent>) -> Self {
        Self {
            name: agent.name().to_string(),
            agent,
            input_mapper: Box::new(default_input_mapper),
            output_mapper: Box::new(default_output_mapper),
        }
    }

    pub fn with_input_mapper<F>(mut self, mapper: F) -> Self
    where
        F: Fn(&State) -> Content + Send + Sync + 'static,
    {
        self.input_mapper = Box::new(mapper);
        self
    }

    pub fn with_output_mapper<F>(mut self, mapper: F) -> Self
    where
        F: Fn(&Event) -> HashMap<String, Value> + Send + Sync + 'static,
    {
        self.output_mapper = Box::new(mapper);
        self
    }
}

/// Subgraph node - embeds another graph as a node
pub struct SubgraphNode {
    name: String,
    graph: CompiledGraph,
    /// Transform parent state to subgraph input
    input_transform: Option<Box<dyn Fn(&State) -> State + Send + Sync>>,
    /// Transform subgraph output to parent state updates
    output_transform: Option<Box<dyn Fn(&State) -> HashMap<String, Value> + Send + Sync>>,
}
```

### 3. Edge System

```rust
/// Target of an edge
#[derive(Clone)]
pub enum EdgeTarget {
    /// Specific node
    Node(String),
    /// End of graph
    End,
}

/// Edge type
pub enum Edge {
    /// Direct edge: always go from source to target
    Direct {
        source: String,
        target: EdgeTarget,
    },

    /// Conditional edge: route based on state
    Conditional {
        source: String,
        /// Router function returns target node name or END
        router: Arc<dyn Fn(&State) -> String + Send + Sync>,
        /// Map of route names to targets (for validation)
        targets: HashMap<String, EdgeTarget>,
    },

    /// Entry edge: from START to first node(s)
    Entry {
        targets: Vec<String>,
    },
}

/// Special node identifiers
pub const START: &str = "__start__";
pub const END: &str = "__end__";

/// Router function builder for common patterns
pub struct Router;

impl Router {
    /// Route based on a state field value
    pub fn by_field(field: &str) -> impl Fn(&State) -> String + Send + Sync + Clone {
        let field = field.to_string();
        move |state: &State| {
            state.get(&field)
                .and_then(|v| v.as_str())
                .unwrap_or(END)
                .to_string()
        }
    }

    /// Route based on presence of tool calls in messages
    pub fn has_tool_calls(
        messages_field: &str,
        if_true: &str,
        if_false: &str,
    ) -> impl Fn(&State) -> String + Send + Sync + Clone {
        let messages_field = messages_field.to_string();
        let if_true = if_true.to_string();
        let if_false = if_false.to_string();
        move |state: &State| {
            let has_calls = state.get(&messages_field)
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.last())
                .and_then(|msg| msg.get("tool_calls"))
                .map(|tc| !tc.as_array().map(|a| a.is_empty()).unwrap_or(true))
                .unwrap_or(false);
            if has_calls { if_true.clone() } else { if_false.clone() }
        }
    }
}
```

### 4. StateGraph Builder

```rust
/// Builder for constructing graphs
pub struct StateGraph {
    schema: StateSchema,
    nodes: HashMap<String, Arc<dyn Node>>,
    edges: Vec<Edge>,
}

impl StateGraph {
    /// Create a new graph with the given state schema
    pub fn new(schema: StateSchema) -> Self {
        Self {
            schema,
            nodes: HashMap::new(),
            edges: vec![],
        }
    }

    /// Create with a simple schema (just channel names, all overwrite)
    pub fn with_channels(channels: &[&str]) -> Self {
        let mut builder = StateSchema::builder();
        for channel in channels {
            builder = builder.channel(channel);
        }
        Self::new(builder.build())
    }

    /// Add a node
    pub fn add_node<N: Node + 'static>(mut self, node: N) -> Self {
        self.nodes.insert(node.name().to_string(), Arc::new(node));
        self
    }

    /// Add a function as a node
    pub fn add_node_fn<F, Fut>(self, name: &str, func: F) -> Self
    where
        F: Fn(&NodeContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<NodeOutput>> + Send + 'static,
    {
        self.add_node(FunctionNode::new(name, func))
    }

    /// Add an existing ADK Agent as a node
    pub fn add_agent_node(self, agent: Arc<dyn Agent>) -> Self {
        self.add_node(AgentNode::new(agent))
    }

    /// Add a direct edge
    pub fn add_edge(mut self, source: &str, target: &str) -> Self {
        let target = if target == END {
            EdgeTarget::End
        } else {
            EdgeTarget::Node(target.to_string())
        };

        if source == START {
            self.edges.push(Edge::Entry {
                targets: vec![match &target {
                    EdgeTarget::Node(n) => n.clone(),
                    EdgeTarget::End => panic!("Cannot go from START to END"),
                }],
            });
        } else {
            self.edges.push(Edge::Direct {
                source: source.to_string(),
                target,
            });
        }
        self
    }

    /// Add a conditional edge with explicit target mapping
    pub fn add_conditional_edges<F>(
        mut self,
        source: &str,
        router: F,
        targets: HashMap<&str, &str>,
    ) -> Self
    where
        F: Fn(&State) -> String + Send + Sync + 'static,
    {
        let targets: HashMap<String, EdgeTarget> = targets
            .into_iter()
            .map(|(k, v)| {
                let target = if v == END {
                    EdgeTarget::End
                } else {
                    EdgeTarget::Node(v.to_string())
                };
                (k.to_string(), target)
            })
            .collect();

        self.edges.push(Edge::Conditional {
            source: source.to_string(),
            router: Arc::new(router),
            targets,
        });
        self
    }

    /// Compile the graph for execution
    pub fn compile(self) -> Result<CompiledGraph> {
        // Validate graph structure
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

    fn validate(&self) -> Result<()> {
        // Check all edge targets exist
        // Check for dangling nodes
        // Check entry points exist
        // Check no duplicate node names
        Ok(())
    }
}
```

### 5. Compiled Graph and Execution

```rust
/// A compiled graph ready for execution
pub struct CompiledGraph {
    schema: StateSchema,
    nodes: HashMap<String, Arc<dyn Node>>,
    edges: Vec<Edge>,
    checkpointer: Option<Arc<dyn Checkpointer>>,
    interrupt_before: HashSet<String>,
    interrupt_after: HashSet<String>,
    recursion_limit: usize,
}

impl CompiledGraph {
    /// Configure checkpointing
    pub fn with_checkpointer(mut self, checkpointer: Arc<dyn Checkpointer>) -> Self {
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

    /// Execute the graph synchronously
    pub async fn invoke(
        &self,
        input: State,
        config: ExecutionConfig,
    ) -> Result<State> {
        let mut executor = PregelExecutor::new(self, config);
        executor.run(input).await
    }

    /// Execute with streaming
    pub fn stream(
        &self,
        input: State,
        config: ExecutionConfig,
        mode: StreamMode,
    ) -> impl Stream<Item = Result<StreamEvent>> {
        let executor = PregelExecutor::new(self, config);
        executor.run_stream(input, mode)
    }

    /// Convert to an ADK Agent
    pub fn into_agent(self, name: &str) -> GraphAgent {
        GraphAgent::new(name, Arc::new(self))
    }

    /// Get current state for a thread
    pub async fn get_state(&self, config: &ExecutionConfig) -> Result<Option<State>> {
        if let Some(cp) = &self.checkpointer {
            cp.load(&config.thread_id).await
        } else {
            Ok(None)
        }
    }

    /// Update state for a thread (for human-in-the-loop)
    pub async fn update_state(
        &self,
        config: &ExecutionConfig,
        updates: HashMap<String, Value>,
    ) -> Result<()> {
        if let Some(cp) = &self.checkpointer {
            let mut state = cp.load(&config.thread_id).await?.unwrap_or_default();
            for (key, value) in updates {
                self.apply_update(&mut state, &key, value);
            }
            cp.save(&config.thread_id, &state).await?;
        }
        Ok(())
    }
}
```

### 6. Pregel Execution Engine

```rust
/// Execution engine based on Pregel model
pub struct PregelExecutor<'a> {
    graph: &'a CompiledGraph,
    config: ExecutionConfig,
    state: State,
    step: usize,
    pending_nodes: HashSet<String>,
}

impl<'a> PregelExecutor<'a> {
    pub fn new(graph: &'a CompiledGraph, config: ExecutionConfig) -> Self {
        Self {
            graph,
            config,
            state: State::new(),
            step: 0,
            pending_nodes: HashSet::new(),
        }
    }

    /// Run the graph to completion
    pub async fn run(&mut self, input: State) -> Result<State> {
        // Initialize state from input or checkpoint
        self.state = self.initialize_state(input).await?;

        // Find entry nodes
        self.pending_nodes = self.get_entry_nodes();

        // Main execution loop
        while !self.pending_nodes.is_empty() {
            // Check recursion limit
            if self.step >= self.graph.recursion_limit {
                return Err(AdkError::RecursionLimitExceeded(self.step));
            }

            // Execute super-step
            let result = self.execute_super_step().await?;

            // Handle interrupts
            if let Some(interrupt) = result.interrupt {
                self.save_checkpoint().await?;
                return Err(AdkError::Interrupted(interrupt));
            }

            // Save checkpoint after each step
            self.save_checkpoint().await?;

            // Determine next nodes
            self.pending_nodes = self.get_next_nodes(&result.executed_nodes);
            self.step += 1;
        }

        Ok(self.state.clone())
    }

    /// Execute one super-step (plan -> execute -> update)
    async fn execute_super_step(&mut self) -> Result<SuperStepResult> {
        let mut result = SuperStepResult::default();

        // Check for interrupt_before
        for node_name in &self.pending_nodes {
            if self.graph.interrupt_before.contains(node_name) {
                return Ok(SuperStepResult {
                    interrupt: Some(Interrupt::Before(node_name.clone())),
                    ..Default::default()
                });
            }
        }

        // Execute all pending nodes in parallel
        let futures: Vec<_> = self.pending_nodes.iter()
            .filter_map(|name| self.graph.nodes.get(name))
            .map(|node| {
                let ctx = NodeContext {
                    state: self.state.clone(),
                    config: self.config.clone(),
                    checkpointer: self.graph.checkpointer.clone(),
                    store: None,
                };
                async move {
                    let output = node.execute(&ctx).await?;
                    Ok::<_, AdkError>((node.name().to_string(), output))
                }
            })
            .collect();

        let outputs: Vec<_> = futures::future::try_join_all(futures).await?;

        // Apply all updates atomically
        let mut all_updates = Vec::new();
        for (node_name, output) in outputs {
            result.executed_nodes.push(node_name.clone());

            // Check for dynamic interrupt
            if let Some(interrupt) = output.interrupt {
                return Ok(SuperStepResult {
                    interrupt: Some(interrupt),
                    executed_nodes: result.executed_nodes,
                    ..Default::default()
                });
            }

            all_updates.push(output.updates);
            result.events.extend(output.events);
        }

        // Apply updates using reducers
        for updates in all_updates {
            for (key, value) in updates {
                self.apply_update(&key, value);
            }
        }

        // Check for interrupt_after
        for node_name in &result.executed_nodes {
            if self.graph.interrupt_after.contains(node_name) {
                return Ok(SuperStepResult {
                    interrupt: Some(Interrupt::After(node_name.clone())),
                    ..result
                });
            }
        }

        Ok(result)
    }

    /// Apply an update using the appropriate reducer
    fn apply_update(&mut self, key: &str, value: Value) {
        let reducer = self.graph.schema.channels
            .get(key)
            .map(|c| &c.reducer)
            .unwrap_or(&Reducer::Overwrite);

        let current = self.state.get(key).cloned().unwrap_or(Value::Null);
        let new_value = match reducer {
            Reducer::Overwrite => value,
            Reducer::Append => {
                let mut arr = current.as_array().cloned().unwrap_or_default();
                if let Some(items) = value.as_array() {
                    arr.extend(items.clone());
                } else {
                    arr.push(value);
                }
                Value::Array(arr)
            }
            Reducer::Sum => {
                let current_num = current.as_f64().unwrap_or(0.0);
                let add_num = value.as_f64().unwrap_or(0.0);
                json!(current_num + add_num)
            }
            Reducer::Custom(f) => f(current, value),
        };
        self.state.insert(key.to_string(), new_value);
    }

    /// Get next nodes based on edges
    fn get_next_nodes(&self, executed: &[String]) -> HashSet<String> {
        let mut next = HashSet::new();

        for edge in &self.graph.edges {
            match edge {
                Edge::Direct { source, target } if executed.contains(source) => {
                    if let EdgeTarget::Node(n) = target {
                        next.insert(n.clone());
                    }
                }
                Edge::Conditional { source, router, targets } if executed.contains(source) => {
                    let route = router(&self.state);
                    if let Some(target) = targets.get(&route) {
                        if let EdgeTarget::Node(n) = target {
                            next.insert(n.clone());
                        }
                    }
                }
                _ => {}
            }
        }

        next
    }
}
```

### 7. Checkpointing

```rust
/// Checkpoint data structure
#[derive(Serialize, Deserialize, Clone)]
pub struct Checkpoint {
    pub thread_id: String,
    pub checkpoint_id: String,
    pub state: State,
    pub step: usize,
    pub pending_nodes: Vec<String>,
    pub metadata: HashMap<String, Value>,
    pub created_at: DateTime<Utc>,
}

/// Checkpointer trait for persistence
#[async_trait]
pub trait Checkpointer: Send + Sync {
    /// Save a checkpoint
    async fn save(&self, thread_id: &str, state: &State) -> Result<String>;

    /// Load the latest checkpoint for a thread
    async fn load(&self, thread_id: &str) -> Result<Option<Checkpoint>>;

    /// Load a specific checkpoint
    async fn load_by_id(&self, checkpoint_id: &str) -> Result<Option<Checkpoint>>;

    /// List all checkpoints for a thread (for time travel)
    async fn list(&self, thread_id: &str) -> Result<Vec<Checkpoint>>;
}

/// In-memory checkpointer for development
pub struct MemoryCheckpointer {
    checkpoints: Arc<RwLock<HashMap<String, Vec<Checkpoint>>>>,
}

/// SQLite checkpointer for production
pub struct SqliteCheckpointer {
    pool: SqlitePool,
}

impl SqliteCheckpointer {
    pub async fn new(database_url: &str) -> Result<Self> {
        let pool = SqlitePool::connect(database_url).await?;

        // Create table
        sqlx::query(r#"
            CREATE TABLE IF NOT EXISTS checkpoints (
                id TEXT PRIMARY KEY,
                thread_id TEXT NOT NULL,
                state TEXT NOT NULL,
                step INTEGER NOT NULL,
                pending_nodes TEXT NOT NULL,
                metadata TEXT,
                created_at TEXT NOT NULL
            )
        "#).execute(&pool).await?;

        sqlx::query(r#"
            CREATE INDEX IF NOT EXISTS idx_thread_id ON checkpoints(thread_id)
        "#).execute(&pool).await?;

        Ok(Self { pool })
    }
}
```

### 8. Human-in-the-Loop

```rust
/// Interrupt request from a node or configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Interrupt {
    /// Interrupt before executing a node
    Before(String),
    /// Interrupt after executing a node
    After(String),
    /// Dynamic interrupt from within a node
    Dynamic {
        message: String,
        data: Option<Value>,
    },
}

/// Result when graph execution is interrupted
pub struct InterruptedExecution {
    pub thread_id: String,
    pub checkpoint_id: String,
    pub interrupt: Interrupt,
    pub state: State,
}

impl InterruptedExecution {
    /// Resume execution with optional new input
    pub async fn resume(
        self,
        graph: &CompiledGraph,
        input: Option<Value>,
    ) -> Result<State> {
        // Update state with user input if provided
        if let Some(input) = input {
            graph.update_state(
                &ExecutionConfig { thread_id: self.thread_id.clone(), ..Default::default() },
                [("user_input".to_string(), input)].into_iter().collect(),
            ).await?;
        }

        // Resume from checkpoint
        graph.invoke(
            State::new(), // Will load from checkpoint
            ExecutionConfig {
                thread_id: self.thread_id,
                resume_from: Some(self.checkpoint_id),
                ..Default::default()
            },
        ).await
    }
}

/// Helper for creating dynamic interrupts in nodes
pub fn interrupt(message: &str) -> NodeOutput {
    NodeOutput::new().with_interrupt(Interrupt::Dynamic {
        message: message.to_string(),
        data: None,
    })
}

pub fn interrupt_with_data(message: &str, data: Value) -> NodeOutput {
    NodeOutput::new().with_interrupt(Interrupt::Dynamic {
        message: message.to_string(),
        data: Some(data),
    })
}
```

### 9. Streaming

```rust
/// Stream mode options
#[derive(Clone, Copy)]
pub enum StreamMode {
    /// Full state after each super-step
    Values,
    /// Only state changes
    Updates,
    /// LLM tokens and messages
    Messages,
    /// Custom events from nodes
    Custom,
    /// Debug information
    Debug,
}

/// Events emitted during streaming
#[derive(Clone, Serialize)]
pub enum StreamEvent {
    /// State snapshot
    State(State),
    /// State updates from a node
    Updates {
        node: String,
        updates: HashMap<String, Value>,
    },
    /// Message/token from LLM
    Message {
        node: String,
        content: String,
        is_final: bool,
    },
    /// Custom event from node
    Custom {
        node: String,
        data: Value,
    },
    /// Debug event
    Debug {
        event_type: String,
        data: Value,
    },
    /// Node started execution
    NodeStart(String),
    /// Node completed execution
    NodeEnd(String),
    /// Super-step completed
    StepComplete(usize),
    /// Graph execution completed
    Done(State),
    /// Error occurred
    Error(String),
}
```

### 10. GraphAgent - ADK Integration

```rust
/// GraphAgent wraps a CompiledGraph as an ADK Agent
pub struct GraphAgent {
    name: String,
    description: String,
    graph: Arc<CompiledGraph>,
    /// Map InvocationContext to graph input state
    input_mapper: Box<dyn Fn(&dyn InvocationContext) -> State + Send + Sync>,
    /// Map graph output state to ADK Event
    output_mapper: Box<dyn Fn(&State) -> Event + Send + Sync>,
}

impl GraphAgent {
    pub fn new(name: &str, graph: Arc<CompiledGraph>) -> Self {
        Self {
            name: name.to_string(),
            description: String::new(),
            graph,
            input_mapper: Box::new(default_context_to_state),
            output_mapper: Box::new(default_state_to_event),
        }
    }

    pub fn with_description(mut self, desc: &str) -> Self {
        self.description = desc.to_string();
        self
    }

    pub fn with_input_mapper<F>(mut self, mapper: F) -> Self
    where
        F: Fn(&dyn InvocationContext) -> State + Send + Sync + 'static,
    {
        self.input_mapper = Box::new(mapper);
        self
    }

    pub fn with_output_mapper<F>(mut self, mapper: F) -> Self
    where
        F: Fn(&State) -> Event + Send + Sync + 'static,
    {
        self.output_mapper = Box::new(mapper);
        self
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

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let input = (self.input_mapper)(ctx.as_ref());
        let config = ExecutionConfig {
            thread_id: ctx.session_id().to_string(),
            ..Default::default()
        };

        let graph = self.graph.clone();
        let output_mapper = self.output_mapper.clone();

        let stream = async_stream::stream! {
            match graph.invoke(input, config).await {
                Ok(state) => {
                    let event = output_mapper(&state);
                    yield Ok(event);
                }
                Err(AdkError::Interrupted(interrupt)) => {
                    yield Ok(Event::interrupted(interrupt));
                }
                Err(e) => {
                    yield Err(e);
                }
            }
        };

        Ok(Box::pin(stream))
    }
}
```

## Example Usage

### Basic ReAct Agent

```rust
use adk_graph::prelude::*;
use adk_agent::LlmAgent;

#[tokio::main]
async fn main() -> Result<()> {
    // Define state schema
    let schema = StateSchema::builder()
        .list_channel("messages")
        .channel("next_action")
        .build();

    // Create LLM agent for reasoning
    let llm = Arc::new(LlmAgent::builder("reasoner")
        .model(model)
        .instruction("You are a ReAct agent. Reason about the task and decide next action.")
        .build()?);

    // Build graph
    let graph = StateGraph::new(schema)
        .add_agent_node(llm)
        .add_node_fn("tools", |ctx| async move {
            let tool_calls = extract_tool_calls(&ctx.state);
            let results = execute_tools(tool_calls).await?;
            Ok(NodeOutput::new()
                .with_update("messages", results))
        })
        .add_edge(START, "reasoner")
        .add_conditional_edges(
            "reasoner",
            Router::has_tool_calls("messages", "tools", END),
            [("tools", "tools"), (END, END)].into(),
        )
        .add_edge("tools", "reasoner")  // Cycle back
        .compile()?
        .with_recursion_limit(25);

    // Execute
    let result = graph.invoke(
        [("messages".to_string(), json!([{"role": "user", "content": "What's the weather?"}]))].into(),
        ExecutionConfig::new("thread_1".to_string()),
    ).await?;

    println!("Result: {:?}", result);
    Ok(())
}
```

### Multi-Agent Supervisor

```rust
use adk_graph::prelude::*;

let graph = StateGraph::with_channels(&["messages", "next_agent", "results"])
    // Supervisor decides which agent to use
    .add_agent_node(supervisor_agent)
    // Specialist agents
    .add_agent_node(research_agent)
    .add_agent_node(writer_agent)
    .add_agent_node(reviewer_agent)
    // Routing
    .add_edge(START, "supervisor")
    .add_conditional_edges(
        "supervisor",
        Router::by_field("next_agent"),
        [
            ("research", "research_agent"),
            ("writer", "writer_agent"),
            ("reviewer", "reviewer_agent"),
            ("done", END),
        ].into(),
    )
    // All agents report back to supervisor
    .add_edge("research_agent", "supervisor")
    .add_edge("writer_agent", "supervisor")
    .add_edge("reviewer_agent", "supervisor")
    .compile()?;
```

### Human-in-the-Loop Approval

```rust
use adk_graph::prelude::*;

let graph = StateGraph::with_channels(&["messages", "pending_action", "approved"])
    .add_node_fn("plan", |ctx| async move {
        let action = plan_action(&ctx.state).await?;
        Ok(NodeOutput::new()
            .with_update("pending_action", action))
    })
    .add_node_fn("execute", |ctx| async move {
        let action = ctx.state.get("pending_action").unwrap();
        let result = execute_action(action).await?;
        Ok(NodeOutput::new()
            .with_update("messages", json!([{"role": "assistant", "content": result}])))
    })
    .add_edge(START, "plan")
    .add_edge("plan", "execute")
    .add_edge("execute", END)
    .compile()?
    .with_checkpointer(Arc::new(SqliteCheckpointer::new("state.db").await?))
    .with_interrupt_after(&["plan"]); // Pause for approval

// First run - will pause after planning
let config = ExecutionConfig::new("approval_thread".to_string());
let result = graph.invoke(input, config.clone()).await;

match result {
    Err(AdkError::Interrupted(interrupted)) => {
        println!("Pending action: {:?}", interrupted.state.get("pending_action"));

        // User approves...
        let final_result = interrupted.resume(&graph, Some(json!({"approved": true}))).await?;
        println!("Final result: {:?}", final_result);
    }
    Ok(state) => println!("Completed without interrupt: {:?}", state),
    Err(e) => return Err(e),
}
```

## Crate Structure

```
adk-graph/
├── Cargo.toml
├── src/
│   ├── lib.rs              # Public API and prelude
│   ├── state.rs            # State, StateSchema, Reducer
│   ├── node.rs             # Node trait, FunctionNode, AgentNode
│   ├── edge.rs             # Edge types, Router
│   ├── graph.rs            # StateGraph builder
│   ├── compiled.rs         # CompiledGraph
│   ├── executor.rs         # PregelExecutor
│   ├── checkpoint.rs       # Checkpointer trait, implementations
│   ├── interrupt.rs        # Human-in-the-loop types
│   ├── stream.rs           # Streaming types and implementation
│   ├── agent.rs            # GraphAgent (ADK Agent integration)
│   └── error.rs            # Graph-specific errors
├── tests/
│   ├── basic_test.rs
│   ├── cycles_test.rs
│   ├── checkpoint_test.rs
│   └── interrupt_test.rs
└── examples/
    ├── react_agent.rs
    ├── multi_agent.rs
    ├── approval_flow.rs
    └── streaming.rs
```

## Dependencies

```toml
[package]
name = "adk-graph"
version = "0.1.3"
edition = "2021"

[dependencies]
adk-core = { version = "0.1", path = "../adk-core" }
async-trait = "0.1"
async-stream = "0.3"
futures = "0.3"
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
chrono = { version = "0.4", features = ["serde"] }
uuid = { version = "1", features = ["v4"] }
thiserror = "1"

[dependencies.sqlx]
version = "0.7"
features = ["runtime-tokio", "sqlite"]
optional = true

[features]
default = []
sqlite = ["sqlx"]
```

## Summary

This design brings LangGraph's power to ADK-Rust while maintaining:

1. **ADK Integration**: GraphAgent implements `Agent` trait, works with existing runners
2. **Type Safety**: Rust's type system with flexible dynamic state where needed
3. **Streaming**: Native EventStream support, multiple stream modes
4. **Production Ready**: Checkpointing, persistence, human-in-the-loop
5. **Composability**: Agents as nodes, subgraphs, conditional routing
6. **Performance**: Parallel node execution, efficient state management
