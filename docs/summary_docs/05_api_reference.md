# API Reference

Complete API reference for all ADK-Rust crates.

## Table of Contents

- [adk-core](#adk-core) - Core traits and types
- [adk-agent](#adk-agent) - Agent implementations  
- [adk-model](#adk-model) - Model integrations
- [adk-tool](#adk-tool) - Tool system
- [adk-session](#adk-session) - Session management
- [adk-artifact](#adk-artifact) - Artifact storage
- [adk-memory](#adk-memory) - Memory system
- [adk-runner](#adk-runner) - Execution runtime
- [adk-server](#adk-server) - HTTP servers
- [adk-cli](#adk-cli) - Command-line interface

---

## adk-core

Core traits and types used across all ADK crates.

### Traits

#### Agent

Main agent abstraction.

```rust
#[async_trait]
pub trait Agent: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn tools(&self) -> &[Arc<dyn Tool>];
    fn sub_agents(&self) -> &[Arc<dyn Agent>];
    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream>;
}
```

#### Llm

Language model abstraction.

```rust
#[async_trait]
pub trait Llm: Send + Sync {
    fn name(&self) -> &str;
    async fn generate_content(&self, req: &LlmRequest) -> Result<LlmResponse>;
    async fn generate_content_stream(&self, req: &LlmRequest) -> Result<LlmResponseStream>;
}
```

#### Tool

Tool/capability abstraction.

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn is_long_running(&self) -> bool { false }
    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value>;
}
```

#### SessionService

Session storage abstraction.

```rust
#[async_trait]
pub trait SessionService: Send + Sync {
    async fn create(&self, req: CreateRequest) -> Result<Box<dyn Session>>;
    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>>;
    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>>;
    async fn delete(&self, req: DeleteRequest) -> Result<()>;
}
```

### Types

#### Content

Represents user or agent messages.

```rust
pub struct Content {
    pub role: String,
    pub parts: Vec<Part>,
}

impl Content {
    pub fn text(text: &str) -> Self;
    pub fn from_parts(parts: Vec<Part>) -> Self;
}
```

#### Part

Individual content pieces.

```rust
pub enum Part {
    Text(String),
    InlineData { mime_type: String, data: Vec<u8> },
    FileData { mime_type: String, file_uri: String },
    FunctionCall { name: String, args: Value },
    FunctionResponse { name: String, response: Value },
}

impl Part {
    pub fn text(&self) -> Option<&str>;
    pub fn is_text(&self) -> bool;
}
```

#### Event

Agent execution events.

```rust
pub struct Event {
    pub invocation_id: String,
    pub agent_name: String,
    pub content: Option<Content>,
    pub actions: EventActions,
}

pub struct EventActions {
    pub pending_tool_calls: Vec<ToolCall>,
    pub tool_calls: Vec<ToolCall>,
    pub transfer: Option<Transfer>,
}
```

#### AdkError

Error type.

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
    
    // ... more variants
}

pub type Result<T> = std::result::Result<T, AdkError>;
```

---

## adk-agent

Agent implementations.

### LlmAgent

LLM-powered agent.

```rust
pub struct LlmAgent { /* fields private */ }

impl LlmAgent {
    // Use LlmAgentBuilder instead of direct construction
}
```

#### LlmAgentBuilder

Builder for LlmAgent.

```rust
pub struct LlmAgentBuilder { /* fields private */ }

impl LlmAgentBuilder {
    pub fn new(name: &str) -> Self;
    
    pub fn description(self, description: &str) -> Self;
    pub fn instruction(self, instruction: &str) -> Self;
    pub fn model(self, model: Arc<dyn Llm>) -> Self;
    pub fn tool(self, tool: Arc<dyn Tool>) -> Self;
    pub fn toolset(self, toolset: Arc<dyn Toolset>) -> Self;
    pub fn sub_agent(self, agent: Arc<dyn Agent>) -> Self;
    
    pub fn before_agent_callback(self, callback: Arc<dyn BeforeAgentCallback>) -> Self;
    pub fn after_agent_callback(self, callback: Arc<dyn AfterAgentCallback>) -> Self;
    pub fn before_model_callback(self, callback: Arc<dyn BeforeModelCallback>) -> Self;
    pub fn after_model_callback(self, callback: Arc<dyn AfterModelCallback>) -> Self;
    pub fn before_tool_callback(self, callback: Arc<dyn BeforeToolCallback>) -> Self;
    pub fn after_tool_callback(self, callback: Arc<dyn AfterToolCallback>) -> Self;
    
    pub fn build(self) -> Result<LlmAgent>;
}
```

**Example**:
```rust
let agent = LlmAgentBuilder::new("assistant")
    .description("Helpful AI assistant")
    .instruction("Be concise and accurate")
    .model(Arc::new(model))
    .tool(Arc::new(search_tool))
    .build()?;
```

### CustomAgent

User-defined agent.

```rust
pub struct CustomAgent { /* fields private */ }
```

#### CustomAgentBuilder

```rust
pub struct CustomAgentBuilder { /* fields private */ }

impl CustomAgentBuilder {
    pub fn new(name: &str) -> Self;
    pub fn description(self, description: &str) -> Self;
    pub fn handler<F>(self, handler: F) -> Self
    where
        F: Fn(Arc<dyn InvocationContext>) -> BoxFuture<'static, Result<Vec<Event>>> 
            + Send + Sync + 'static;
    pub fn build(self) -> Result<CustomAgent>;
}
```

**Example**:
```rust
let agent = CustomAgentBuilder::new("custom")
    .description("Custom logic")
    .handler(|ctx| async move {
        let response = Content::text("Custom response");
        Ok(vec![Event::new(ctx.invocation_id(), "custom", Some(response))])
    })
    .build()?;
```

### SequentialAgent

Sequential workflow agent.

```rust
pub struct SequentialAgent { /* fields private */ }

impl SequentialAgent {
    pub fn new(name: &str, agents: Vec<Arc<dyn Agent>>) -> Self;
    pub fn with_description(name: &str, description: &str, agents: Vec<Arc<dyn Agent>>) -> Self;
}
```

### ParallelAgent

Parallel workflow agent.

```rust
pub struct ParallelAgent { /* fields private */ }

impl ParallelAgent {
    pub fn new(name: &str, agents: Vec<Arc<dyn Agent>>) -> Self;
    pub fn with_description(name: &str, description: &str, agents: Vec<Arc<dyn Agent>>) -> Self;
}
```

### LoopAgent

Iterative loop agent.

```rust
pub struct LoopAgent { /* fields private */ }

impl LoopAgent {
    pub fn new(
        name: &str,
        agent: Arc<dyn Agent>,
        max_iterations: Option<usize>,
    ) -> Result<Self>;
    
    pub fn with_description(
        name: &str,
        description: &str,
        agent: Arc<dyn Agent>,
        max_iterations: Option<usize>,
    ) -> Result<Self>;
}
```

### ConditionalAgent

Conditional branching agent.

```rust
pub struct ConditionalAgent { /* fields private */ }

impl ConditionalAgent {
    pub fn new<F>(
        name: &str,
        condition: F,
        true_agent: Arc<dyn Agent>,
        false_agent: Arc<dyn Agent>,
    ) -> Result<Self>
    where
        F: Fn(Arc<dyn InvocationContext>) -> BoxFuture<'static, Result<bool>> 
            + Send + Sync + 'static;
}
```

---

## adk-model

Model integrations.

### GeminiModel

Google Gemini model.

```rust
pub struct GeminiModel { /* fields private */ }

impl GeminiModel {
    pub fn new(api_key: &str, model_name: &str) -> Result<Self>;
    
    pub fn with_config(
        api_key: &str,
        model_name: &str,
        config: GenerateContentConfig,
    ) -> Result<Self>;
}
```

**Supported models**:
- `gemini-2.0-flash-exp`
- `gemini-1.5-pro`
- `gemini-1.5-flash`

**Example**:
```rust
let model = GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?;
```

### MockLlm

Mock model for testing.

```rust
pub struct MockLlm { /* fields private */ }

impl MockLlm {
    pub fn new(response: &str) -> Self;
}
```

---

## adk-tool

Tool system and built-in tools.

### FunctionTool

Create tools from functions.

```rust
pub struct FunctionTool { /* fields private */ }

impl FunctionTool {
    pub fn new<F>(name: &str, description: &str, handler: F) -> Self
    where
        F: Fn(Arc<dyn ToolContext>, Value) -> BoxFuture<'static, Result<Value>> 
            + Send + Sync + 'static;
}
```

**Example**:
```rust
let tool = FunctionTool::new(
    "calculate",
    "Perform arithmetic calculations",
    |_ctx, args| async move {
        let a = args["a"].as_f64().unwrap();
        let b = args["b"].as_f64().unwrap();
        Ok(json!({"result": a + b}))
    }
);
```

### GoogleSearchTool

Google search integration.

```rust
pub struct GoogleSearchTool { /* fields private */ }

impl GoogleSearchTool {
    pub fn new() -> Self;
}
```

### ExitLoopTool

Exit loop iterations.

```rust
pub struct ExitLoopTool { /* fields private */ }

impl ExitLoopTool {
    pub fn new() -> Self;
}
```

### LoadArtifactsTool

Load artifacts from storage.

```rust
pub struct LoadArtifactsTool { /* fields private */ }

impl LoadArtifactsTool {
    pub fn new() -> Self;
}
```

### BasicToolset

Group tools together.

```rust
pub struct BasicToolset { /* fields private */ }

impl BasicToolset {
    pub fn new(name: &str, tools: Vec<Arc<dyn Tool>>) -> Self;
}
```

### McpToolset

Model Context Protocol integration.

```rust
pub struct McpToolset { /* fields private */ }

impl McpToolset {
    pub fn new(name: &str, config: McpConfig) -> Result<Self>;
}
```

---

## adk-session

Session management.

### InMemorySessionService

In-memory session storage.

```rust
pub struct InMemorySessionService { /* fields private */ }

impl InMemorySessionService {
    pub fn new() -> Self;
}
```

### DatabaseSessionService

SQLite-backed session storage.

```rust
pub struct DatabaseSessionService { /* fields private */ }

impl DatabaseSessionService {
    pub async fn new(database_url: &str) -> Result<Self>;
}
```

---

## adk-runner

Execution runtime.

### Runner

Agent execution engine.

```rust
pub struct Runner { /* fields private */ }

impl Runner {
    pub fn new(
        app_name: &str,
        agent: Arc<dyn Agent>,
        session_service: Arc<dyn SessionService>,
    ) -> Self;
    
    pub fn builder() -> RunnerBuilder;
    
    pub async fn run(
        &self,
        user_id: String,
        session_id: String,
        content: Content,
    ) -> Result<EventStream>;
}
```

### RunnerBuilder

```rust
pub struct RunnerBuilder { /* fields private */ }

impl RunnerBuilder {
    pub fn app_name(self, name: &str) -> Self;
    pub fn agent(self, agent: Arc<dyn Agent>) -> Self;
    pub fn session_service(self, service: Arc<dyn SessionService>) -> Self;
    pub fn artifact_service(self, service: Arc<dyn ArtifactService>) -> Self;
    pub fn memory_service(self, service: Arc<dyn MemoryService>) -> Self;
    pub fn build(self) -> Result<Runner>;
}
```

---

## adk-server

HTTP server implementations.

### ServerConfig

```rust
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    pub app_name: String,
}
```

### REST API

Start REST server:

```rust
pub async fn start_server(
    config: ServerConfig,
    agent: Arc<dyn Agent>,
    session_service: Arc<dyn SessionService>,
) -> Result<()>;
```

### Endpoints

#### POST /api/run

Run agent with input.

**Request**:
```json
{
  "userId": "user123",
  "sessionId": "session456",
  "message": "Hello"
}
```

**Response**: SSE stream of events

#### GET /health

Health check.

**Response**:
```json
{
  "status": "ok"
}
```

---

## adk-cli

Command-line interface.

### Commands

#### console

Interactive console mode.

```bash
adk console --agent-name <NAME>
```

#### serve

Start HTTP server.

```bash
adk serve --port <PORT>
```

---

## Complete Example

Putting it all together:

```rust
use adk_agent::LlmAgentBuilder;
use adk_core::{Agent, Content};
use adk_model::gemini::GeminiModel;
use adk_runner::Runner;
use adk_session::InMemorySessionService;
use adk_tool::{FunctionTool, GoogleSearchTool};
use std::sync::Arc;
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Create model
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?);
    
    // 2. Create tools
    let search = Arc::new(GoogleSearchTool::new());
    let calculator = Arc::new(FunctionTool::new(
        "calculate",
        "Add two numbers",
        |_ctx, args| async move {
            let result = args["a"].as_f64().unwrap() + args["b"].as_f64().unwrap();
            Ok(serde_json::json!({"result": result}))
        }
    ));
    
    // 3. Build agent
    let agent = LlmAgentBuilder::new("assistant")
        .description("Helpful assistant with search and calculation")
        .model(model)
        .tool(search)
        .tool(calculator)
        .build()?;
    
    // 4. Create runner
    let session_service = Arc::new(InMemorySessionService::new());
    let runner = Runner::new("my-app", Arc::new(agent), session_service);
    
    // 5. Run query
    let query = Content::text("What is 42 + 58?");
    let mut events = runner.run("user1".into(), "sess1".into(), query).await?;
    
    // 6. Process events
    while let Some(event) = events.next().await {
        let evt = event?;
        if let Some(content) = evt.content {
            println!("{:?}", content);
        }
    }
    
    Ok(())
}
```

---

**Previous**: [Core Concepts](04_concepts.md) | **Next**: [MCP Integration](06_mcp.md)
