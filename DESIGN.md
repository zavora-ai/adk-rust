# ADK-Rust Design Document

## Architecture Overview

ADK-Rust follows a modular, trait-based architecture that mirrors the Go implementation while leveraging Rust's type system and ownership model.

```
┌─────────────────────────────────────────────────────────────┐
│                        Application Layer                     │
│  (CLI, REST Server, A2A Server, Examples)                   │
└─────────────────────────────────────────────────────────────┘
                              │
┌─────────────────────────────────────────────────────────────┐
│                         Runner Layer                         │
│  (Agent Execution, Context Management, Event Streaming)      │
└─────────────────────────────────────────────────────────────┘
                              │
┌─────────────────────────────────────────────────────────────┐
│                         Agent Layer                          │
│  (Agent Trait, LLMAgent, WorkflowAgents, CustomAgent)       │
└─────────────────────────────────────────────────────────────┘
                              │
┌──────────────┬──────────────┬──────────────┬────────────────┐
│   Model      │    Tool      │   Session    │   Services     │
│   Layer      │    Layer     │   Layer      │   Layer        │
│              │              │              │                │
│ - LLM Trait  │ - Tool Trait │ - Session    │ - Artifact     │
│ - Gemini     │ - Function   │ - Events     │ - Memory       │
│ - Streaming  │ - Toolset    │ - State      │ - Storage      │
└──────────────┴──────────────┴──────────────┴────────────────┘
```

## Core Design Principles

### 1. Trait-Based Abstraction
- Use traits for all major abstractions (Agent, Tool, LLM, Session, etc.)
- Enable extensibility through trait implementations
- Support dynamic dispatch where flexibility is needed
- Use static dispatch for performance-critical paths

### 2. Async-First Design
- All I/O operations are async
- Use `async_trait` for async trait methods
- Leverage Tokio's async runtime
- Support streaming with `futures::Stream`

### 3. Type Safety
- Use strong typing to prevent invalid states
- Leverage Rust's type system for compile-time guarantees
- Use newtypes for domain concepts (SessionId, AgentName, etc.)
- Minimize `unwrap()` and handle errors explicitly

### 4. Ownership & Borrowing
- Use `Arc` for shared ownership across async tasks
- Use `Arc<RwLock<T>>` or `Arc<Mutex<T>>` for shared mutable state
- Prefer immutable data structures where possible
- Use `Cow` for efficient string handling

## Module Structure

```
adk-rust/
├── adk-core/              # Core traits and types
│   ├── agent.rs           # Agent trait and types
│   ├── model.rs           # LLM trait and types
│   ├── tool.rs            # Tool trait and types
│   ├── session.rs         # Session types
│   ├── context.rs         # Context types
│   └── error.rs           # Error types
│
├── adk-agent/             # Agent implementations
│   ├── llm_agent.rs       # LLM-based agent
│   ├── custom_agent.rs    # Custom agent wrapper
│   ├── workflow/          # Workflow agents
│   │   ├── sequential.rs
│   │   ├── parallel.rs
│   │   └── loop_agent.rs
│   └── remote_agent.rs    # A2A remote agent
│
├── adk-model/             # Model implementations
│   ├── gemini/            # Gemini integration
│   │   ├── client.rs
│   │   ├── streaming.rs
│   │   └── types.rs
│   └── mock.rs            # Mock for testing
│
├── adk-tool/              # Tool implementations
│   ├── function_tool.rs   # Function-based tools
│   ├── agent_tool.rs      # Agent as tool
│   ├── builtin/           # Built-in tools
│   │   ├── google_search.rs
│   │   └── exit_loop.rs
│   └── mcp/               # MCP integration
│
├── adk-session/           # Session management
│   ├── service.rs         # Session service trait
│   ├── inmemory.rs        # In-memory implementation
│   ├── database.rs        # Database implementation
│   ├── event.rs           # Event types
│   └── state.rs           # State management
│
├── adk-artifact/          # Artifact management
│   ├── service.rs         # Artifact service trait
│   ├── inmemory.rs        # In-memory storage
│   └── gcs.rs             # GCS storage
│
├── adk-memory/            # Memory system
│   ├── service.rs         # Memory service trait
│   └── inmemory.rs        # In-memory implementation
│
├── adk-runner/            # Execution runtime
│   ├── runner.rs          # Main runner
│   ├── context.rs         # Invocation context
│   └── callbacks.rs       # Callback system
│
├── adk-server/            # Server implementations
│   ├── rest/              # REST API
│   │   ├── handler.rs
│   │   ├── routes.rs
│   │   └── controllers/
│   └── a2a/               # A2A protocol
│       ├── handler.rs
│       └── protocol.rs
│
├── adk-cli/               # CLI application
│   ├── main.rs
│   ├── console.rs
│   └── launcher.rs
│
└── examples/              # Example applications
    ├── quickstart.rs
    ├── tools.rs
    └── workflow.rs
```

## Key Design Decisions

### D-1: Agent Trait Design

```rust
#[async_trait]
pub trait Agent: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn sub_agents(&self) -> &[Arc<dyn Agent>];
    
    async fn run(
        &self,
        ctx: Arc<InvocationContext>,
    ) -> Result<impl Stream<Item = Result<Event>>>;
}
```

**Rationale**: 
- `async_trait` for async methods
- `Arc` for shared ownership of sub-agents
- Stream for event streaming
- `Send + Sync` for thread safety

### D-2: Error Handling

```rust
#[derive(Debug, thiserror::Error)]
pub enum AdkError {
    #[error("Agent error: {0}")]
    Agent(String),
    
    #[error("Model error: {0}")]
    Model(String),
    
    #[error("Tool error: {0}")]
    Tool(String),
    
    #[error("Session error: {0}")]
    Session(String),
    
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, AdkError>;
```

**Rationale**:
- Use `thiserror` for ergonomic error definitions
- Categorize errors by domain
- Support error conversion with `#[from]`

### D-3: Context Management

```rust
pub struct InvocationContext {
    invocation_id: String,
    session: Arc<RwLock<dyn MutableSession>>,
    agent: Arc<dyn Agent>,
    artifacts: Option<Arc<dyn ArtifactService>>,
    memory: Option<Arc<dyn MemoryService>>,
    user_content: Content,
    run_config: RunConfig,
}
```

**Rationale**:
- Immutable context with interior mutability for session
- Arc for shared ownership across async tasks
- Optional services for flexibility

### D-4: Streaming Design

```rust
pub type EventStream = Pin<Box<dyn Stream<Item = Result<Event>> + Send>>;

impl Agent for LlmAgent {
    async fn run(&self, ctx: Arc<InvocationContext>) -> Result<EventStream> {
        let stream = async_stream::stream! {
            // Generate events
            yield Ok(event);
        };
        Ok(Box::pin(stream))
    }
}
```

**Rationale**:
- Use `futures::Stream` for async iteration
- `Pin<Box<...>>` for heap allocation and pinning
- `async_stream` crate for ergonomic stream creation

### D-5: Tool System

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn is_long_running(&self) -> bool { false }
    
    async fn execute(
        &self,
        ctx: Arc<ToolContext>,
        args: serde_json::Value,
    ) -> Result<serde_json::Value>;
}

// Function tool with type safety
pub struct FunctionTool<F, Args, Output> {
    name: String,
    description: String,
    handler: F,
    _phantom: PhantomData<(Args, Output)>,
}

impl<F, Args, Output> FunctionTool<F, Args, Output>
where
    F: Fn(Arc<ToolContext>, Args) -> BoxFuture<'static, Result<Output>> + Send + Sync,
    Args: DeserializeOwned + Send,
    Output: Serialize + Send,
{
    pub fn new(name: String, description: String, handler: F) -> Self {
        Self {
            name,
            description,
            handler,
            _phantom: PhantomData,
        }
    }
}
```

**Rationale**:
- Generic function tool for type-safe handlers
- JSON for dynamic tool arguments
- Async execution with `BoxFuture`

### D-6: Session Storage

```rust
#[async_trait]
pub trait SessionService: Send + Sync {
    async fn get(&self, req: &GetRequest) -> Result<Session>;
    async fn create(&self, req: &CreateRequest) -> Result<Session>;
    async fn append_event(&self, session: &Session, event: Event) -> Result<()>;
    async fn update_state(&self, session: &Session, key: String, value: Value) -> Result<()>;
}

pub struct InMemorySessionService {
    sessions: Arc<RwLock<HashMap<String, SessionData>>>,
}
```

**Rationale**:
- Trait for pluggable storage backends
- Arc<RwLock<...>> for concurrent access
- Separate in-memory and database implementations

### D-7: Builder Pattern

```rust
pub struct LlmAgentBuilder {
    name: String,
    description: String,
    model: Option<Arc<dyn Llm>>,
    tools: Vec<Arc<dyn Tool>>,
    instruction: Option<String>,
    sub_agents: Vec<Arc<dyn Agent>>,
}

impl LlmAgentBuilder {
    pub fn new(name: impl Into<String>) -> Self { ... }
    pub fn description(mut self, desc: impl Into<String>) -> Self { ... }
    pub fn model(mut self, model: Arc<dyn Llm>) -> Self { ... }
    pub fn tool(mut self, tool: Arc<dyn Tool>) -> Self { ... }
    pub fn build(self) -> Result<LlmAgent> { ... }
}
```

**Rationale**:
- Ergonomic API for complex configurations
- Compile-time validation where possible
- Runtime validation in `build()`

### D-8: Callback System

```rust
pub type BeforeAgentCallback = Box<dyn Fn(Arc<CallbackContext>) -> BoxFuture<'static, Result<Option<Content>>> + Send + Sync>;

pub type AfterAgentCallback = Box<dyn Fn(Arc<CallbackContext>, &Event) -> BoxFuture<'static, Result<Option<Content>>> + Send + Sync>;
```

**Rationale**:
- Boxed closures for flexibility
- Async callbacks with `BoxFuture`
- Optional return to modify flow

## Concurrency Model

### Agent Execution
- Single-threaded per invocation (sequential event generation)
- Parallel tool execution within an agent turn
- Concurrent session access with read-write locks

### Workflow Agents
- **Sequential**: Execute sub-agents one at a time
- **Parallel**: Use `tokio::spawn` for concurrent execution
- **Loop**: Iterate with async loop

### Streaming
- Use `tokio::sync::mpsc` for event channels
- Buffer events to prevent backpressure
- Support cancellation via context

## Data Flow

### User Request Flow
```
User Input
    ↓
Runner.run()
    ↓
Find Agent (from session history)
    ↓
Create InvocationContext
    ↓
Agent.run(ctx)
    ↓
[LLM Agent Flow]
    ↓
Generate Content (LLM)
    ↓
Process Function Calls
    ↓
Execute Tools (parallel)
    ↓
Aggregate Responses
    ↓
Generate Events (stream)
    ↓
Save to Session
    ↓
Return to User
```

### Event Streaming
```
Agent.run() → Stream<Event>
    ↓
Runner buffers events
    ↓
Save non-partial events to session
    ↓
Yield to caller
    ↓
REST/CLI/A2A handler
    ↓
User receives events
```

## Testing Strategy

### Unit Tests
- Test each trait implementation independently
- Mock dependencies with test implementations
- Use `tokio::test` for async tests

### Integration Tests
- Test agent execution end-to-end
- Use in-memory services for isolation
- Test streaming behavior

### Example Tests
- Ensure all examples compile and run
- Use as smoke tests for API changes

## Performance Considerations

### Memory Management
- Use `Arc` to avoid cloning large structures
- Stream events to avoid buffering entire conversations
- Implement `Drop` for cleanup where needed

### Async Efficiency
- Minimize task spawning overhead
- Use `tokio::select!` for concurrent operations
- Avoid blocking operations in async context

### Serialization
- Use `serde` with efficient formats (bincode for internal, JSON for API)
- Lazy deserialization where possible
- Zero-copy where applicable

## Security Considerations

### Input Validation
- Validate all user inputs
- Sanitize file paths for artifacts
- Limit session/state sizes

### API Keys
- Never log API keys
- Support environment variables and secure storage
- Implement key rotation support

### Sandboxing
- Isolate tool execution where possible
- Implement timeouts for long-running operations
- Resource limits for memory/CPU

## Migration from Go ADK

### API Mapping
- Go interfaces → Rust traits
- Go channels → Rust streams/channels
- Go goroutines → Tokio tasks
- Go context → Rust context structs

### Key Differences
- Explicit error handling (no panics)
- Ownership instead of GC
- Async/await instead of goroutines
- Trait objects instead of interfaces

### Compatibility
- Maintain same REST/A2A protocols
- Support same session/artifact formats
- Compatible with same LLM providers
