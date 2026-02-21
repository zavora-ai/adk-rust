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
adk-core = "0.3"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = "0.3"
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
    InlineData { mime_type: String, data: Vec<u8> },  // Max 10MB (MAX_INLINE_DATA_SIZE)
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

// Provider-specific metadata (replaces GCP-specific fields)
event.provider_metadata  // HashMap<String, String>
```

### EventActions

```rust
pub struct EventActions {
    pub state_delta: HashMap<String, Value>,  // State updates
    pub artifact_delta: HashMap<String, i64>, // Artifact changes
    pub skip_summarization: bool,
    pub transfer_to_agent: Option<String>,    // Agent transfer
    pub escalate: bool,                       // Escalate to parent
    pub tool_confirmation: Option<ToolConfirmationRequest>,  // Pending tool confirmation
    pub tool_confirmation_decision: Option<ToolConfirmationDecision>,
    pub compaction: Option<EventCompaction>,  // Context compaction summary
}
```

### EventCompaction

When context compaction is enabled, older events are summarized into a single compacted event:

```rust
pub struct EventCompaction {
    pub start_timestamp: DateTime<Utc>,   // Earliest compacted event
    pub end_timestamp: DateTime<Utc>,     // Latest compacted event
    pub compacted_content: Content,       // The summary replacing original events
}
```

### High-Stakes Tooling & Context Engineering

For production agents, `adk-core` provides types to ensure that an agent's instructions (the cognitive frame) always match its tool capabilities (the physical frame).

#### `ResolvedContext`
An "atomic unit" containing the final system instruction and the collection of verified, binary `Arc<dyn Tool>` instances. This prevents "Phantom Tool" hallucinations.

#### `ToolRegistry`
A foundational trait for mapping string-based tool names (from config or skills) to concrete executable tool instances.

#### `ValidationMode`
Defines how the framework handles cases where a requested tool is missing:
- **Strict**: Rejects the match/operation if any tool is missing.
- **Permissive**: Binds available tools, omits missing ones, and logs a warning.

---

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

## Security

### Inline Data Size Limit

`Content::with_inline_data()` and `Part::inline_data()` enforce a 10 MB limit (`MAX_INLINE_DATA_SIZE`) to prevent oversized payloads.

### State Key Validation

`validate_state_key()` rejects keys that are empty, exceed 256 bytes (`MAX_STATE_KEY_LEN`), contain path separators (`/`, `\`, `..`), or null bytes.

```rust
use adk_core::context::validate_state_key;

assert!(validate_state_key("user_name").is_ok());
assert!(validate_state_key("../etc/passwd").is_err());
```

### Provider Metadata

The `Event` struct uses a generic `provider_metadata: HashMap<String, String>` field for provider-specific data (e.g., GCP Vertex, Azure OpenAI), keeping the core type provider-agnostic.

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

State keys are validated with `validate_state_key()` — max length is `MAX_STATE_KEY_LEN` (256 bytes), and keys must be valid UTF-8 with no control characters.

## Callbacks

```rust
// Callback type aliases
pub type BeforeAgentCallback = Box<dyn Fn(...) -> ... + Send + Sync>;
pub type AfterAgentCallback = Box<dyn Fn(...) -> ... + Send + Sync>;
pub type BeforeModelCallback = Box<dyn Fn(...) -> ... + Send + Sync>;
pub type AfterModelCallback = Box<dyn Fn(...) -> ... + Send + Sync>;
pub type BeforeToolCallback = Box<dyn Fn(...) -> ... + Send + Sync>;
pub type AfterToolCallback = Box<dyn Fn(...) -> ... + Send + Sync>;

// Instruction providers
pub type InstructionProvider = Box<dyn Fn(...) -> ... + Send + Sync>;
pub type GlobalInstructionProvider = Box<dyn Fn(...) -> ... + Send + Sync>;
```

## Context Compaction

Types for sliding-window context compaction (summarizing older events to reduce LLM context size):

```rust
/// Trait for summarizing events during compaction.
#[async_trait]
pub trait BaseEventsSummarizer: Send + Sync {
    async fn summarize_events(&self, events: &[Event]) -> Result<Option<Event>>;
}

/// Configuration for automatic context compaction.
pub struct EventsCompactionConfig {
    pub compaction_interval: u32,  // Invocations between compactions
    pub overlap_size: u32,         // Events to carry over for continuity
    pub summarizer: Arc<dyn BaseEventsSummarizer>,
}
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
