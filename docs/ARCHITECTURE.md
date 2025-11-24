# ADK-Rust Architecture

## Overview

ADK-Rust is a layered architecture for building AI agents with Rust. It provides abstractions for agents, models, tools, sessions, and execution.

## System Layers

```
┌─────────────────────────────────────────────────────────────┐
│                    Application Layer                         │
│         (CLI, REST Server, A2A Server, Examples)            │
└─────────────────────────────────────────────────────────────┘
                              │
┌─────────────────────────────────────────────────────────────┐
│                       Runner Layer                           │
│    (Agent Execution, Context Management, Event Streaming)    │
└─────────────────────────────────────────────────────────────┘
                              │
┌─────────────────────────────────────────────────────────────┐
│                       Agent Layer                            │
│   (Agent Trait, LLMAgent, WorkflowAgents, CustomAgent)      │
└─────────────────────────────────────────────────────────────┘
                              │
┌──────────────┬──────────────┬──────────────┬────────────────┐
│   Model      │    Tool      │   Session    │   Services     │
│   Layer      │    Layer     │   Layer      │   Layer        │
└──────────────┴──────────────┴──────────────┴────────────────┘
```

## Core Concepts

### 1. Agent

The `Agent` trait is the core abstraction:

```rust
#[async_trait]
pub trait Agent: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn tools(&self) -> &[Arc<dyn Tool>];
    fn sub_agents(&self) -> &[Arc<dyn Agent>];
    async fn run(&self, ctx: Arc<dyn InvocationContext>) 
        -> Result<EventStream>;
}
```

**Agent Types**:
- **LlmAgent**: Uses LLM for responses
- **CustomAgent**: User-defined logic
- **SequentialAgent**: Chains agents
- **ParallelAgent**: Runs agents concurrently
- **LoopAgent**: Iterative execution

### 2. Model

The `Llm` trait abstracts language models:

```rust
#[async_trait]
pub trait Llm: Send + Sync {
    fn name(&self) -> &str;
    async fn generate_content(&self, req: &LlmRequest) 
        -> Result<LlmResponse>;
    async fn generate_content_stream(&self, req: &LlmRequest) 
        -> Result<LlmResponseStream>;
}
```

**Implementations**:
- **GeminiModel**: Google Gemini integration

### 3. Tool

The `Tool` trait enables function calling:

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn is_long_running(&self) -> bool;
    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) 
        -> Result<Value>;
}
```

**Built-in Tools**:
- GoogleSearchTool
- ExitLoopTool
- LoadArtifactsTool
- FunctionTool (custom functions)

**Toolsets**:
- BasicToolset: Groups tools
- McpToolset: MCP server integration

### 4. Session

Sessions manage conversation state:

```rust
#[async_trait]
pub trait SessionService: Send + Sync {
    async fn create(&self, req: CreateRequest) -> Result<Box<dyn Session>>;
    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>>;
    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>>;
    async fn delete(&self, req: DeleteRequest) -> Result<()>;
}
```

**Implementations**:
- InMemorySessionService
- DatabaseSessionService (SQLite)

### 5. Runner

The `Runner` orchestrates agent execution:

```rust
pub struct Runner {
    app_name: String,
    agent: Arc<dyn Agent>,
    session_service: Arc<dyn SessionService>,
    artifact_service: Option<Arc<dyn ArtifactService>>,
    memory_service: Option<Arc<dyn MemoryService>>,
}

impl Runner {
    pub async fn run(&self, user_id: String, session_id: String, 
                     content: Content) -> Result<EventStream>;
}
```

## Data Flow

### Request Flow

```
User Input
    │
    ├─> Runner.run()
    │       │
    │       ├─> Load Session
    │       ├─> Create Context
    │       └─> Agent.run()
    │               │
    │               ├─> Model.generate_content()
    │               │       │
    │               │       └─> Tool.execute() (if function call)
    │               │
    │               └─> Stream Events
    │
    └─> Event Stream
```

### Event Streaming

Events flow through async streams:

```rust
pub struct Event {
    pub invocation_id: String,
    pub agent_name: String,
    pub content: Option<Content>,
    pub actions: EventActions,
}

pub type EventStream = Pin<Box<dyn Stream<Item = Result<Event>> + Send>>;
```

## Key Design Patterns

### 1. Trait-Based Polymorphism

All major components use traits for extensibility:
- Agent, Llm, Tool, SessionService, etc.
- Enables custom implementations
- Facilitates testing with mocks

### 2. Builder Pattern

Complex objects use builders:

```rust
let agent = LlmAgentBuilder::new("agent")
    .description("...")
    .model(Arc::new(model))
    .tool(Arc::new(tool))
    .build()?;
```

### 3. Async/Await

All I/O is async with Tokio:
- Non-blocking operations
- Efficient resource usage
- Streaming responses

### 4. Arc for Shared Ownership

Services and agents use `Arc<T>`:
- Thread-safe reference counting
- Shared across async tasks
- No runtime overhead

### 5. Type-Safe Errors

Custom error types with `thiserror`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum AdkError {
    #[error("Agent error: {0}")]
    Agent(String),
    #[error("Model error: {0}")]
    Model(String),
    // ...
}
```

## Module Organization

### adk-core
Core traits and types used by all modules.

**Key exports**:
- Agent, Llm, Tool, Toolset traits
- Content, Part, Event types
- AdkError, Result

### adk-agent
Agent implementations.

**Key exports**:
- LlmAgent, LlmAgentBuilder
- CustomAgent
- SequentialAgent, ParallelAgent, LoopAgent

### adk-model
Model integrations.

**Key exports**:
- GeminiModel
- LlmRequest, LlmResponse

### adk-tool
Tool implementations.

**Key exports**:
- FunctionTool
- GoogleSearchTool, ExitLoopTool, LoadArtifactsTool
- BasicToolset, McpToolset

### adk-session
Session management.

**Key exports**:
- SessionService trait
- InMemorySessionService, DatabaseSessionService
- Session, State, Events

### adk-artifact
Artifact storage.

**Key exports**:
- ArtifactService trait
- InMemoryArtifactService, DatabaseArtifactService

### adk-memory
Memory system.

**Key exports**:
- MemoryService trait
- InMemoryMemoryService

### adk-runner
Execution runtime.

**Key exports**:
- Runner, RunnerConfig
- InvocationContext

### adk-server
HTTP servers.

**Key exports**:
- REST API (Axum)
- A2A protocol support
- ServerConfig

### adk-cli
Command-line interface.

**Key exports**:
- Console mode (rustyline)
- Server launcher

## Concurrency Model

### Async Runtime

Uses Tokio for async execution:
- Multi-threaded work-stealing scheduler
- Efficient I/O with epoll/kqueue
- Async mutexes for shared state

### Thread Safety

All public types are `Send + Sync`:
- Safe to share across threads
- No data races
- Enforced by compiler

### Streaming

Events stream via `futures::Stream`:
- Backpressure support
- Lazy evaluation
- Composable with stream combinators

## Error Handling

### Error Propagation

Uses `Result<T, AdkError>` throughout:
- Explicit error handling
- No panics in library code
- Errors carry context

### Error Types

```rust
pub enum AdkError {
    Agent(String),
    Model(String),
    Tool(String),
    Session(String),
    Artifact(String),
    Memory(String),
    Runner(String),
    Server(String),
}
```

## Testing Strategy

### Unit Tests

Each module has unit tests:
- Test individual functions
- Mock dependencies
- Fast execution

### Integration Tests

Test component interactions:
- End-to-end scenarios
- Real dependencies
- Slower but comprehensive

### Example Tests

Examples serve as integration tests:
- Verify real-world usage
- Documentation by example

## Performance Considerations

### Zero-Cost Abstractions

Traits compile to direct calls:
- No vtable overhead for monomorphized code
- Inlining opportunities

### Minimal Allocations

Careful memory management:
- Reuse buffers where possible
- Stream data instead of buffering
- Arc for shared data

### Async Efficiency

Tokio provides:
- Efficient task scheduling
- Minimal context switching
- I/O multiplexing

## Security

### Memory Safety

Rust guarantees:
- No buffer overflows
- No use-after-free
- No data races

### Input Validation

All user inputs validated:
- Type checking at compile time
- Runtime validation for external data
- Sanitization of untrusted input

### Dependency Auditing

Regular security audits:
- `cargo audit` for known vulnerabilities
- Minimal dependency tree
- Trusted crates only

## Extensibility

### Custom Agents

Implement `Agent` trait:

```rust
struct MyAgent;

#[async_trait]
impl Agent for MyAgent {
    // Implement required methods
}
```

### Custom Tools

Implement `Tool` trait or use `FunctionTool`:

```rust
let tool = FunctionTool::new("my_tool", "description", handler);
```

### Custom Models

Implement `Llm` trait:

```rust
struct MyModel;

#[async_trait]
impl Llm for MyModel {
    // Implement required methods
}
```

## Future Enhancements

- Additional model providers (OpenAI, Anthropic)
- More workflow patterns
- Advanced memory strategies
- Distributed execution
- Performance optimizations
