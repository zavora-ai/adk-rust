# adk-core

Core traits and types for ADK-Rust agents, tools, sessions, and events.

[![Crates.io](https://img.shields.io/crates/v/adk-core.svg)](https://crates.io/crates/adk-core)
[![Documentation](https://docs.rs/adk-core/badge.svg)](https://docs.rs/adk-core)
[![License](https://img.shields.io/crates/l/adk-core.svg)](LICENSE)

## Overview

`adk-core` provides the foundational abstractions for [ADK-Rust](https://github.com/zavora-ai/adk-rust). It defines the core traits and types that all other ADK crates build upon:

- **Agent trait** - The fundamental abstraction for all agents
- **Tool / Toolset traits** - For extending agents with custom capabilities
- **Llm trait** - For LLM provider integrations
- **Context hierarchy** - ReadonlyContext → CallbackContext → ToolContext/InvocationContext
- **Content / Part** - Message content structures
- **Event system** - For streaming agent responses
- **Session / State** - For managing conversation context
- **Error types** - Unified error handling

This crate is model-agnostic and contains no LLM-specific code.

## Installation

```toml
[dependencies]
adk-core = "0.1"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = "0.1"
```

## Core Traits

### Agent

```rust
#[async_trait]
pub trait Agent: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn sub_agents(&self) -> &[Arc<dyn Agent>];
    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream>;
}
```

### Tool

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> Option<Value>;
    fn is_long_running(&self) -> bool;
    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value>;
}
```

### Toolset

```rust
#[async_trait]
pub trait Toolset: Send + Sync {
    fn name(&self) -> &str;
    async fn tools(&self, ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>>;
}
```

### Llm

```rust
#[async_trait]
pub trait Llm: Send + Sync {
    fn name(&self) -> &str;
    async fn generate_content(&self, request: LlmRequest, stream: bool) -> Result<LlmResponseStream>;
}
```

## Key Types

### Content & Part

```rust
// Message content with role and parts
let content = Content::new("user")
    .with_text("Hello!")
    .with_inline_data("image/png", image_bytes)
    .with_file_uri("image/jpeg", "https://example.com/image.jpg");

// Part variants
enum Part {
    Text { text: String },
    InlineData { mime_type: String, data: Vec<u8> },
    FileData { mime_type: String, file_uri: String },
    FunctionCall { name: String, args: Value, id: Option<String> },
    FunctionResponse { function_response: FunctionResponseData, id: Option<String> },
}
```

### Event

```rust
// Events stream from agent execution
let event = Event::new("invocation_123");
event.content()  // Access response content
event.actions    // State changes, transfers, escalation
```

### EventActions

```rust
pub struct EventActions {
    pub state_delta: HashMap<String, Value>,  // State updates
    pub artifact_delta: HashMap<String, i64>, // Artifact changes
    pub skip_summarization: bool,
    pub transfer_to_agent: Option<String>,    // Agent transfer
    pub escalate: bool,                       // Escalate to parent
}
```

## Context Hierarchy

```
ReadonlyContext (read-only access)
    ├── invocation_id, agent_name, user_id, app_name, session_id
    └── user_content()

CallbackContext (extends ReadonlyContext)
    └── artifacts()

ToolContext (extends CallbackContext)
    ├── function_call_id()
    ├── actions() / set_actions()
    └── search_memory()

InvocationContext (extends CallbackContext)
    ├── agent(), memory(), session()
    ├── run_config()
    └── end_invocation() / ended()
```

## State Management

State uses typed prefixes for organization:

| Prefix | Scope | Persistence |
|--------|-------|-------------|
| `user:` | User preferences | Across sessions |
| `app:` | Application state | Application-wide |
| `temp:` | Temporary data | Cleared each turn |

```rust
// Access state via session
let value = session.state().get("user:preference");
session.state().set("temp:counter".to_string(), json!(42));
```

## Callbacks

```rust
// Callback type aliases
pub type BeforeAgentCallback = Arc<dyn Fn(...) -> ... + Send + Sync>;
pub type AfterAgentCallback = Arc<dyn Fn(...) -> ... + Send + Sync>;
pub type BeforeModelCallback = Arc<dyn Fn(...) -> ... + Send + Sync>;
pub type AfterModelCallback = Arc<dyn Fn(...) -> ... + Send + Sync>;
pub type BeforeToolCallback = Arc<dyn Fn(...) -> ... + Send + Sync>;
pub type AfterToolCallback = Arc<dyn Fn(...) -> ... + Send + Sync>;

// Instruction providers
pub type InstructionProvider = Arc<dyn Fn(...) -> ... + Send + Sync>;
pub type GlobalInstructionProvider = Arc<dyn Fn(...) -> ... + Send + Sync>;
```

## Streaming Modes

```rust
pub enum StreamingMode {
    None,  // Complete responses only
    SSE,   // Server-Sent Events (default)
    Bidi,  // Bidirectional (realtime)
}
```

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-agent](https://crates.io/crates/adk-agent) - Agent implementations
- [adk-model](https://crates.io/crates/adk-model) - LLM integrations
- [adk-tool](https://crates.io/crates/adk-tool) - Tool implementations

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
