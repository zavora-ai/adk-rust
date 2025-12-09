# Development Guidelines

This document provides comprehensive guidelines for developers contributing to ADK-Rust. Following these standards ensures code quality, consistency, and maintainability across the project.

## Table of Contents

- [Getting Started](#getting-started)
- [Project Structure](#project-structure)
- [Code Style](#code-style)
- [Error Handling](#error-handling)
- [Async Patterns](#async-patterns)
- [Testing](#testing)
- [Documentation](#documentation)
- [Pull Request Process](#pull-request-process)
- [Common Tasks](#common-tasks)

## Getting Started

### Prerequisites

- **Rust**: 1.75 or higher (check with `rustc --version`)
- **Cargo**: Latest stable
- **Git**: For version control

### Setting Up Your Environment

```bash
# Clone the repository
git clone https://github.com/anthropics/adk-rust.git
cd adk-rust

# Build the project
cargo build

# Run all tests
cargo test --all

# Check for lints
cargo clippy --all-targets --all-features

# Format code
cargo fmt --all
```

### Environment Variables

For running examples and tests that require API keys:

```bash
# Gemini (default provider)
export GOOGLE_API_KEY="your-api-key"

# OpenAI (optional)
export OPENAI_API_KEY="your-api-key"

# Anthropic (optional)
export ANTHROPIC_API_KEY="your-api-key"
```

## Project Structure

ADK-Rust is organized as a Cargo workspace with multiple crates:

```
adk-rust/
├── adk-core/       # Foundational traits and types (Agent, Tool, Llm, Event)
├── adk-telemetry/  # OpenTelemetry integration
├── adk-model/      # LLM providers (Gemini, OpenAI, Anthropic)
├── adk-tool/       # Tool system (FunctionTool, MCP, AgentTool)
├── adk-session/    # Session management (in-memory, SQLite)
├── adk-artifact/   # Binary artifact storage
├── adk-memory/     # Long-term memory with search
├── adk-agent/      # Agent implementations (LlmAgent, workflow agents)
├── adk-runner/     # Execution runtime
├── adk-server/     # REST API and A2A protocol
├── adk-cli/        # Command-line launcher
├── adk-realtime/   # Voice/audio streaming agents
├── adk-graph/      # LangGraph-style workflows
├── adk-browser/    # Browser automation tools
├── adk-eval/       # Agent evaluation framework
├── adk-rust/       # Umbrella crate (re-exports all)
└── examples/       # Working examples
```

### Crate Dependencies

Crates must be published in dependency order:

1. `adk-core` (no internal deps)
2. `adk-telemetry`
3. `adk-model`
4. `adk-tool`
5. `adk-session`
6. `adk-artifact`
7. `adk-memory`
8. `adk-agent`
9. `adk-runner`
10. `adk-server`
11. `adk-cli`
12. `adk-realtime`
13. `adk-graph`
14. `adk-browser`
15. `adk-eval`
16. `adk-rust` (umbrella)

## Code Style

### General Principles

1. **Clarity over cleverness**: Write code that is easy to read and understand
2. **Explicit over implicit**: Prefer explicit types and error handling
3. **Small functions**: Keep functions focused and under 50 lines when possible
4. **Meaningful names**: Use descriptive variable and function names

### Formatting

Use `rustfmt` with default settings:

```bash
cargo fmt --all
```

The CI pipeline enforces formatting. Always run `cargo fmt` before committing.

### Naming Conventions

| Type | Convention | Example |
|------|------------|---------|
| Crates | `adk-*` (kebab-case) | `adk-core`, `adk-agent` |
| Modules | snake_case | `llm_agent`, `function_tool` |
| Types/Traits | PascalCase | `LlmAgent`, `ToolContext` |
| Functions | snake_case | `execute_tool`, `run_agent` |
| Constants | SCREAMING_SNAKE_CASE | `KEY_PREFIX_APP` |
| Type parameters | Single uppercase or PascalCase | `T`, `State` |

### Imports

Organize imports in this order:

```rust
// 1. Standard library
use std::collections::HashMap;
use std::sync::Arc;

// 2. External crates
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

// 3. Internal crates (adk-*)
use adk_core::{Agent, Event, Result};

// 4. Local modules
use crate::config::Config;
use super::utils;
```

### Clippy

All code must pass clippy with no warnings:

```bash
cargo clippy --all-targets --all-features
```

Address clippy warnings rather than suppressing them. If suppression is necessary, document why:

```rust
#[allow(clippy::too_many_arguments)]
// Builder pattern requires many parameters; refactoring would hurt usability
fn complex_builder(...) { }
```

## Error Handling

### Use `adk_core::AdkError`

All errors should use the centralized error type:

```rust
use adk_core::{AdkError, Result};

// Return Result<T> (aliased to Result<T, AdkError>)
pub async fn my_function() -> Result<String> {
    // Use ? for propagation
    let data = fetch_data().await?;

    // Create errors with appropriate variants
    if data.is_empty() {
        return Err(AdkError::Tool("No data found".into()));
    }

    Ok(data)
}
```

### Error Variants

Use the appropriate error variant:

| Variant | Use Case |
|---------|----------|
| `AdkError::Agent(String)` | Agent execution errors |
| `AdkError::Model(String)` | LLM provider errors |
| `AdkError::Tool(String)` | Tool execution errors |
| `AdkError::Session(String)` | Session management errors |
| `AdkError::Artifact(String)` | Artifact storage errors |
| `AdkError::Config(String)` | Configuration errors |
| `AdkError::Network(String)` | HTTP/network errors |

### Error Messages

Write clear, actionable error messages:

```rust
// Good: Specific and actionable
Err(AdkError::Config("API key not found. Set GOOGLE_API_KEY environment variable.".into()))

// Bad: Vague
Err(AdkError::Config("Invalid config".into()))
```

## Async Patterns

### Use Tokio

All async code uses the Tokio runtime:

```rust
use tokio::sync::{Mutex, RwLock};

// Prefer RwLock for read-heavy data
let state: Arc<RwLock<State>> = Arc::new(RwLock::new(State::default()));

// Use Mutex for write-heavy or simple cases
let counter: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));
```

### Async Traits

Use `async_trait` for async trait methods:

```rust
use async_trait::async_trait;

#[async_trait]
pub trait MyTrait: Send + Sync {
    async fn do_work(&self) -> Result<()>;
}
```

### Streaming

Use `EventStream` for streaming responses:

```rust
use adk_core::EventStream;
use async_stream::stream;
use futures::Stream;

fn create_stream() -> EventStream {
    let s = stream! {
        yield Ok(Event::new("inv-1"));
        yield Ok(Event::new("inv-2"));
    };
    Box::pin(s)
}
```

### Thread Safety

All public types must be `Send + Sync`:

```rust
// Good: Thread-safe
pub struct MyAgent {
    name: String,
    tools: Vec<Arc<dyn Tool>>,  // Arc for shared ownership
}

// Verify with compile-time checks
fn assert_send_sync<T: Send + Sync>() {}
fn _check() {
    assert_send_sync::<MyAgent>();
}
```

## Testing

### Test Organization

```
crate/
├── src/
│   ├── lib.rs          # Unit tests at bottom of file
│   └── module.rs       # Module-specific tests
└── tests/
    └── integration.rs  # Integration tests
```

### Unit Tests

Place unit tests in the same file as the code:

```rust
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add() {
        assert_eq!(add(2, 3), 5);
    }

    #[tokio::test]
    async fn test_async_function() {
        let result = async_function().await;
        assert!(result.is_ok());
    }
}
```

### Integration Tests

Place in `tests/` directory:

```rust
// tests/integration_test.rs
use adk_core::*;

#[tokio::test]
async fn test_full_workflow() {
    // Setup
    let service = InMemorySessionService::new();

    // Execute
    let session = service.create(request).await.unwrap();

    // Assert
    assert_eq!(session.id(), "test-session");
}
```

### Mock Testing

Use `MockLlm` for testing without API calls:

```rust
use adk_model::MockLlm;

#[tokio::test]
async fn test_agent_with_mock() {
    let mock = MockLlm::new(vec![
        "First response".to_string(),
        "Second response".to_string(),
    ]);

    let agent = LlmAgentBuilder::new("test")
        .model(Arc::new(mock))
        .build()
        .unwrap();

    // Test agent behavior
}
```

### Test Commands

```bash
# Run all tests
cargo test --all

# Run specific crate tests
cargo test --package adk-core

# Run with output
cargo test --all -- --nocapture

# Run ignored tests (require API keys)
cargo test --all -- --ignored
```

## Documentation

### Doc Comments

Use `///` for public items:

```rust
/// Creates a new LLM agent with the specified configuration.
///
/// # Arguments
///
/// * `name` - A unique identifier for this agent
/// * `model` - The LLM provider to use for reasoning
///
/// # Examples
///
/// ```rust
/// use adk_agent::LlmAgentBuilder;
///
/// let agent = LlmAgentBuilder::new("assistant")
///     .model(Arc::new(model))
///     .build()?;
/// ```
///
/// # Errors
///
/// Returns `AdkError::Agent` if the model is not set.
pub fn new(name: impl Into<String>) -> Self {
    // ...
}
```

### Module Documentation

Add module-level docs at the top of `lib.rs`:

```rust
//! # adk-core
//!
//! Core types and traits for ADK-Rust.
//!
//! ## Overview
//!
//! This crate provides the foundational types...
```

### README Files

Each crate should have a `README.md` with:

1. Brief description
2. Installation instructions
3. Quick example
4. Link to full documentation

### Documentation Tests

Ensure doc examples compile:

```bash
cargo test --doc --all
```

## Pull Request Process

### Before Submitting

1. **Run the full test suite**:
   ```bash
   cargo test --all
   ```

2. **Run clippy**:
   ```bash
   cargo clippy --all-targets --all-features
   ```

3. **Format code**:
   ```bash
   cargo fmt --all
   ```

4. **Update documentation** if adding/changing public API

5. **Add tests** for new functionality

### PR Guidelines

- **Title**: Clear, concise description of the change
- **Description**: Explain what and why (not how)
- **Size**: Keep PRs focused; split large changes
- **Tests**: Include tests for new functionality
- **Breaking changes**: Clearly document in description

### Commit Messages

Follow conventional commits:

```
feat: add OpenAI streaming support
fix: correct tool parameter validation
docs: update quickstart guide
refactor: simplify session state management
test: add integration tests for A2A protocol
```

## Common Tasks

### Adding a New Tool

1. **Create the tool**:

```rust
use adk_core::{Tool, ToolContext, Result};
use async_trait::async_trait;
use serde_json::Value;

pub struct MyTool {
    // fields
}

#[async_trait]
impl Tool for MyTool {
    fn name(&self) -> &str {
        "my_tool"
    }

    fn description(&self) -> &str {
        "Does something useful"
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "input": { "type": "string" }
            },
            "required": ["input"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let input = args["input"].as_str().unwrap_or_default();
        Ok(serde_json::json!({ "result": input }))
    }
}
```

2. **Add to agent**:

```rust
let agent = LlmAgentBuilder::new("agent")
    .model(model)
    .tool(Arc::new(MyTool::new()))
    .build()?;
```

### Adding a New Model Provider

1. **Create module** in `adk-model/src/`:

```rust
// adk-model/src/mymodel/mod.rs
mod client;
pub use client::MyModelClient;
```

2. **Implement the Llm trait**:

```rust
use adk_core::{Llm, LlmRequest, LlmResponse, LlmResponseStream, Result};

pub struct MyModelClient {
    api_key: String,
}

#[async_trait]
impl Llm for MyModelClient {
    fn name(&self) -> &str {
        "my-model"
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        stream: bool,
    ) -> Result<LlmResponseStream> {
        // Implementation
    }
}
```

3. **Add feature flag** in `adk-model/Cargo.toml`:

```toml
[features]
mymodel = ["dep:mymodel-sdk"]
```

4. **Export conditionally**:

```rust
#[cfg(feature = "mymodel")]
pub mod mymodel;
#[cfg(feature = "mymodel")]
pub use mymodel::MyModelClient;
```

### Adding a New Agent Type

1. **Create module** in `adk-agent/src/`:

```rust
// adk-agent/src/my_agent.rs
use adk_core::{Agent, EventStream, InvocationContext, Result};
use async_trait::async_trait;

pub struct MyAgent {
    name: String,
}

#[async_trait]
impl Agent for MyAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "My custom agent"
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        // Implementation
    }
}
```

2. **Export** in `adk-agent/src/lib.rs`:

```rust
mod my_agent;
pub use my_agent::MyAgent;
```

### Debugging Tips

1. **Enable tracing**:
   ```rust
   adk_telemetry::init_telemetry();
   ```

2. **Inspect events**:
   ```rust
   while let Some(event) = stream.next().await {
       eprintln!("Event: {:?}", event);
   }
   ```

3. **Use RUST_LOG**:
   ```bash
   RUST_LOG=debug cargo run --example myexample
   ```

---

**Questions?** Open an issue on [GitHub](https://github.com/anthropics/adk-rust/issues).
