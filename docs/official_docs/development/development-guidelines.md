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

- **Rust**: 1.95.0 or higher (edition 2024, check with `rustc --version`)
- **Cargo**: Latest stable
- **Git**: For version control
- **sccache** (recommended): Compilation cache that cuts rebuild times substantially; set `RUSTC_WRAPPER=sccache`

### Setting Up Your Environment

```bash
# Clone the repository
git clone https://github.com/zavora-ai/adk-rust.git
cd adk-rust

# Option A: Nix/devenv (reproducible — identical on Linux, macOS, CI)
devenv shell

# Option B: Setup script (checks the toolchain, cargo-nextest, protoc, NASM, cmake, lefthook)
./scripts/setup-dev.sh            # Linux, macOS
./scripts/setup-dev.sh --check    # report without changing anything
./scripts/setup-dev.ps1           # Windows

# Option C: Manual
cargo build

# Quality gates (run all three before every commit)
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
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

ADK-Rust is organized as a Cargo workspace of `adk-*` crates. The main ones:

```
adk-rust/
├── adk-core/       # Foundational traits and types (Agent, Tool, Llm, Event)
├── adk-telemetry/  # OpenTelemetry integration
├── adk-model/      # LLM providers (Gemini, OpenAI, Anthropic, DeepSeek, Groq, Ollama, ...)
├── adk-tool/       # Tool system (FunctionTool, MCP, AgentTool)
├── adk-session/    # Session management (in-memory, SQLite, PostgreSQL, Redis, MongoDB, ...)
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
└── examples/       # Standalone example crates
```

`AGENTS.md` in the repository root lists every crate.

### Crate Dependencies

Crates are published in dependency order. `cargo xtask publish` (run by `./publish.sh`)
computes the order from the workspace graph, and `scripts/check-publish-order.sh` checks that it
can be satisfied. `adk-core` has no internal dependencies; `adk-rust`, the umbrella crate, is
published last.

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
cargo clippy --workspace --all-targets -- -D warnings
```

A default workspace build compiles no feature-gated module; check a feature-gated change with its
feature, for example `cargo clippy -p adk-agent --features codeact --all-targets -- -D warnings`.

Address clippy warnings rather than suppressing them. If suppression is necessary, document why:

```rust
// A builder needs every parameter; grouping them would hurt usability.
#[allow(clippy::too_many_arguments)]
fn complex_builder(a: u8, b: u8, c: u8, d: u8, e: u8, f: u8, g: u8, h: u8) -> u8 {
    a + b + c + d + e + f + g + h
}
```

## Error Handling

### Structured Error Envelope

`AdkError` is a structured error type with component (where), category (what kind), code (machine key), message (human text), retry hint, and optional details:

```rust
use adk_core::{AdkError, ErrorComponent, ErrorCategory, Result};

// Return Result<T> (aliased to Result<T, AdkError>)
pub async fn my_function() -> Result<String> {
    let data = fetch_data().await?;

    if data.is_empty() {
        return Err(AdkError::new(
            ErrorComponent::Tool,
            ErrorCategory::NotFound,
            "tool.data.not_found",
            "No data found for the given query",
        ));
    }

    Ok(data)
}
```

### Choosing Component and Category

`ErrorComponent` identifies where the failure happened (the origin subsystem, not the trait boundary it surfaces through):

| Component | Use When |
|-----------|----------|
| `Agent` | Agent orchestration, sub-agent dispatch |
| `Model` | LLM provider calls, response parsing |
| `Tool` | Tool execution, parameter validation |
| `Session` | Session persistence, state management |
| `Artifact` | Artifact storage |
| `Memory` | Memory/RAG operations |
| `Graph` | Graph workflow execution |
| `Realtime` | Realtime audio and video sessions |
| `Code` | Code execution |
| `Server` | HTTP server, configuration |
| `Auth` | Authentication, authorization |
| `Guardrail` | Input and output guardrails |
| `Eval` | Evaluation |
| `Deploy` | Deployment clients |

`ErrorCategory` classifies what went wrong:

| Category | HTTP | Use When |
|----------|------|----------|
| `InvalidInput` | 400 | Bad parameters, config, request body |
| `Unauthorized` | 401 | Missing or invalid credentials |
| `Forbidden` | 403 | Valid credentials, insufficient permissions |
| `NotFound` | 404 | Resource doesn't exist |
| `RateLimited` | 429 | Upstream rate limit (retryable) |
| `Timeout` | 408 | Operation exceeded time limit (retryable) |
| `Unavailable` | 503 | Upstream service down (retryable) |
| `Cancelled` | 499 | Cancelled by caller or system |
| `Internal` | 500 | Bugs, invariant violations |
| `Unsupported` | 501 | Feature not supported |
| `ResourceExhausted` | 429 | A configured run budget is used up (not retryable within the run) |
| `Conflict` | 409 | The write lost a race or used a stale version; reload and retry, or stop |

### Convenience Constructors

For common patterns:

```rust
// Structured (preferred for new code)
AdkError::not_found(ErrorComponent::Session, "session.not_found", "Session xyz not found")
AdkError::rate_limited(ErrorComponent::Model, "model.openai.rate_limited", "Too many requests")
AdkError::unauthorized(ErrorComponent::Auth, "auth.token_expired", "Bearer token expired")
AdkError::timeout(ErrorComponent::Tool, "tool.execution_timeout", "Tool timed out after 30s")

// Backward-compatible (for migration — produces .legacy codes)
AdkError::tool("No data found")
AdkError::model("Provider returned 500")
AdkError::session("Session not found")
```

### Builder API

Attach structured metadata for richer error context:

```rust
let err = AdkError::new(
    ErrorComponent::Model,
    ErrorCategory::RateLimited,
    "model.openai.rate_limited",
    "OpenAI rate limit exceeded",
)
.with_provider("openai")
.with_upstream_status(429)
.with_request_id("req-abc123")
.with_retry(RetryHint {
    should_retry: true,
    retry_after_ms: Some(5000),
    max_attempts: Some(3),
});
```

### Retry Hints

Retryable categories (`RateLimited`, `Unavailable`, `Timeout`) automatically set `should_retry: true`. Check retryability with `err.is_retryable()`, which reads `retry.should_retry` as the single source of truth:

```rust
if err.is_retryable() {
    if let Some(delay) = err.retry.retry_after() {
        tokio::time::sleep(delay).await;
    }
    // retry the operation
}
```

### Category Checks

```rust
err.is_retryable()    // retry.should_retry (RateLimited, Unavailable, Timeout by default)
err.is_not_found()    // category == NotFound
err.is_unauthorized() // category == Unauthorized
err.is_rate_limited() // category == RateLimited
err.is_timeout()      // category == Timeout
```

### Component Checks (backward compat)

```rust
err.is_model()   // component == Model
err.is_tool()    // component == Tool
err.is_session() // component == Session
err.is_config()  // code == "config.legacy" (temporary bridge)
```

### HTTP Status and Problem JSON

`AdkError` maps directly to HTTP responses:

```rust
let status = err.http_status_code(); // u16 based on category
let body = err.to_problem_json();    // structured JSON error body
// body: { "error": { "code", "message", "component", "category", "requestId", "retryAfter", ... } }
```

### Crate-Local Errors with From Impls

Crates with domain-specific errors implement `From<CrateLocalError> for AdkError`:

```rust
// In your crate
#[derive(Debug, thiserror::Error)]
pub enum MyToolError {
    #[error("connection failed: {0}")]
    ConnectionFailed(String),
    #[error("timeout after {0}ms")]
    Timeout(u64),
}

impl From<MyToolError> for AdkError {
    fn from(err: MyToolError) -> Self {
        let (category, code) = match &err {
            MyToolError::ConnectionFailed(_) => (ErrorCategory::Unavailable, "mytool.connection"),
            MyToolError::Timeout(_) => (ErrorCategory::Timeout, "mytool.timeout"),
        };
        AdkError::new(ErrorComponent::Tool, category, code, err.to_string())
            .with_source(err)
    }
}
```

### No Blanket From Impls

`std::io::Error` and `serde_json::Error` cross too many subsystem boundaries for a single blanket conversion. Use explicit `map_err` with the correct component:

```rust
// Good: explicit component and category
let data = std::fs::read_to_string(path)
    .map_err(|e| AdkError::new(
        ErrorComponent::Session,
        ErrorCategory::Internal,
        "session.io_read",
        format!("failed to read session file: {e}"),
    ).with_source(e))?;

// Good: for quick migration
let data: serde_json::Value = serde_json::from_str(&raw)
    .map_err(|e| AdkError::session(format!("JSON parse failed: {e}")))?;
```

### Error Messages

Write clear, actionable error messages:

```rust
// Good: specific and actionable
AdkError::new(
    ErrorComponent::Model,
    ErrorCategory::InvalidInput,
    "model.missing_api_key",
    "API key not found. Set GOOGLE_API_KEY environment variable.",
)

// Bad: vague
AdkError::model("Invalid config")
```

### No Panics in Library Code

Library crates must never panic on recoverable errors. Avoid `unwrap()`, `expect()`, and `panic!()` in `src/` code (test code is exempt).

**RwLock / Mutex**: Use graceful degradation instead of `unwrap()`. A poisoned lock means another thread panicked — crashing the current thread too makes things worse.

```rust
// Bad: panics if lock is poisoned
let state = self.state.write().unwrap();

// Good: log and return a safe default
let Ok(state) = self.state.write() else {
    tracing::error!("state lock poisoned — returning default");
    return Default::default();
};

// Good: recover through the poison (data may be stale but won't crash)
let state = self.state.read().unwrap_or_else(|e| e.into_inner());
```

**Constructors**: Return `Result` when initialization can fail (e.g., connecting to external services).

```rust
// Bad: panics if Docker is not running
pub fn new(config: Config) -> Self {
    let client = connect().expect("connection failed");
    Self { client }
}

// Good: caller decides how to handle the failure
pub fn new(config: Config) -> Result<Self, MyError> {
    let client = connect().map_err(|e| MyError::Init(e.to_string()))?;
    Ok(Self { client })
}
```

**Builder methods**: Use `if let Some` instead of `expect()` for `Arc::get_mut()`:

```rust
// Bad: panics if Arc is shared
pub fn add_callback(mut self, cb: Callback) -> Self {
    Arc::get_mut(&mut self.callbacks).expect("not shared").push(cb);
    self
}

// Good: silent no-op (builder pattern guarantees single ownership)
pub fn add_callback(mut self, cb: Callback) -> Self {
    if let Some(callbacks) = Arc::get_mut(&mut self.callbacks) {
        callbacks.push(cb);
    }
    self
}
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

### Tokio Feature Convention

Library crates (`adk-*`) must declare only the minimal tokio features they actually use:

```toml
# Library crates — minimal features
tokio = { workspace = true, features = ["rt", "sync", "time"] }

# Binary crates only (adk-cli, examples) — full is acceptable
tokio = { workspace = true, features = ["full"] }
```

Never use `features = ["full"]` in a library crate. This forces all downstream consumers to compile every tokio subsystem whether they need it or not.

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
use adk_core::{Event, EventStream};
use async_stream::stream;

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

### Test Runner

ADK-Rust uses [cargo-nextest](https://nexte.st/) for test execution. Nextest runs each test binary in a separate process with parallel scheduling, giving ~10x speedup over `cargo test` on this workspace.

```bash
# Install (one-time)
curl -LsSf https://get.nexte.st/latest/mac | tar zxf - -C ${CARGO_HOME:-~/.cargo}/bin

# Or via devenv (included automatically)
devenv shell
```

Configuration lives in `.config/nextest.toml` with two profiles:
- `default` — local development (fail-fast, no retries)
- `ci` — CI runs (retries for flaky tests, slow-test warnings)

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

pub async fn async_function() -> Result<(), String> {
    Ok(())
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
use adk_session::{CreateRequest, InMemorySessionService, SessionService};

#[tokio::test]
async fn test_full_workflow() {
    // Setup
    let service = InMemorySessionService::new();

    // Execute
    let session = service
        .create(CreateRequest {
            app_name: "app".to_string(),
            user_id: "user".to_string(),
            session_id: Some("test-session".to_string()),
            state: Default::default(),
        })
        .await
        .unwrap();

    // Assert
    assert_eq!(session.id(), "test-session");
}
```

### Mock Testing

Use `ScriptedLlm` to test an agent without API calls. It answers each model call with the
next scripted reply and records every request, so a test can script a tool loop and assert
on what the agent sent:

```rust
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::{Content, Part};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_tool::FunctionTool;
use futures::StreamExt;
use serde_json::json;

#[tokio::test]
async fn test_agent_with_scripted_model() {
    let llm = Arc::new(ScriptedLlm::new(
        "scripted",
        [
            // First model call: ask for a tool.
            ScriptedResponse::function_call("get_weather", json!({ "city": "Nairobi" })),
            // Second model call, after the tool result: answer.
            ScriptedResponse::text("It is sunny in Nairobi."),
        ],
    ));
    let get_weather = FunctionTool::new("get_weather", "Weather for a city", |_ctx, args| async move {
        Ok(json!({ "city": args["city"], "condition": "sunny" }))
    });

    let agent = LlmAgentBuilder::new("test")
        .model(llm.clone())
        .tool(Arc::new(get_weather))
        .build()
        .unwrap();
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "test_app".to_string(),
            user_id: "user".to_string(),
            session_id: Some("session-1".to_string()),
            state: Default::default(),
        })
        .await
        .unwrap();
    let runner = Runner::builder()
        .app_name("test_app")
        .agent(Arc::new(agent))
        .session_service(sessions)
        .build()
        .unwrap();

    let mut events = runner
        .run_str("user", "session-1", Content::new("user").with_text("Weather in Nairobi?"))
        .await
        .unwrap();
    let mut answer = String::new();
    while let Some(event) = events.next().await {
        if let Some(content) = event.unwrap().content() {
            for part in &content.parts {
                if let Part::Text { text } = part {
                    answer.push_str(text);
                }
            }
        }
    }

    assert_eq!(answer, "It is sunny in Nairobi.");
    // The second request carries the get_weather function response.
    let requests = llm.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].contents.iter().flat_map(|c| &c.parts).any(|part| {
        matches!(part, Part::FunctionResponse { function_response, .. } if function_response.name == "get_weather")
    }));
}
```

A call past the end of the script fails with `adk_model::SCRIPT_EXHAUSTED_CODE`.
`ScriptedResponse` also scripts streamed chunks, a stream that ends in an error, a failed call,
and reported usage. `MockLlm` replays the same responses on every call, so it suits a single
model turn only.

### Test Commands

```bash
# Run all tests (nextest — parallel, fast)
cargo nextest run --workspace

# Run specific crate tests
cargo nextest run -p adk-core

# Run with CI profile (retries flaky tests)
cargo nextest run --workspace --profile ci

# Run doctests (nextest doesn't run these — use cargo test)
cargo test --workspace --doc

# Run ignored tests (require API keys)
cargo nextest run --workspace --run-ignored only

# Run the PostgreSQL suites (ignored by default) against a server with pgvector
docker run -d -p 5432:5432 -e POSTGRES_PASSWORD=postgres pgvector/pgvector:pg17
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres scripts/test-postgres.sh

# Run with output (nextest shows output for failing tests by default)
cargo nextest run --workspace --no-capture

# Devenv shortcuts
devenv shell ws-test          # nextest, default profile
devenv shell ws-test-ci       # nextest, CI profile
devenv shell ws-test-slow     # cargo test fallback (includes doctests)
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
/// Returns an error with component `Agent` if the model is not set.
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
cargo test --workspace --doc
```

Cargo snippets and feature names in `README.md` files and `docs/official_docs/` are checked by
`scripts/check-doc-examples.sh` and `scripts/check-doc-versions.sh`.

## Pull Request Process

### Before Submitting

1. **Run the full test suite**:
   ```bash
   cargo nextest run --workspace
   ```

2. **Run clippy**:
   ```bash
   cargo clippy --workspace --all-targets -- -D warnings
   ```

3. **Format code**:
   ```bash
   cargo fmt --all -- --check
   ```

4. **Update documentation** if adding/changing public API

5. **Add tests** for new functionality

6. **Add a changelog fragment** under `changelog.d/` for user-facing changes

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

## Project Scaffolding

Use `cargo adk new` with the [Composable Template System](composable-templates.md) to scaffold new projects. The built-in registry provides 13 templates, 10 add-ons, and 5 enterprise patterns:

```bash
# Basic agent (default)
cargo adk new my-agent

# Agent with tools and Docker support
cargo adk new my-agent --template tools --addon docker

# A2A protocol agent with telemetry
cargo adk new my-agent --template a2a --addon telemetry

# Graph workflow with telemetry and a container build
cargo adk new my-agent --template graph --addon telemetry --addon docker
```

The `--addon` flag is composable — combine any base template with any number of addons. See the [Composable Templates](composable-templates.md) documentation for the full list of templates, addons, and enterprise patterns.

## Common Tasks

### Adding a New Tool

1. **Create the tool**:

```rust
use std::sync::Arc;

use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::Value;

pub struct MyTool;

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

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let input = args["input"].as_str().unwrap_or_default();
        Ok(serde_json::json!({ "result": input }))
    }

    // Read-only and concurrency-safe tools can run in parallel with other tool calls.
    fn is_read_only(&self) -> bool {
        true
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }
}
```

2. **Add to agent**:

```rust
let agent = LlmAgentBuilder::new("agent")
    .model(model)
    .tool(Arc::new(MyTool))
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
use adk_core::{Content, Llm, LlmRequest, LlmResponse, LlmResponseStream, Result};
use async_trait::async_trait;

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
        _request: LlmRequest,
        _stream: bool,
    ) -> Result<LlmResponseStream> {
        // Call the provider with `self.api_key` and map its reply into `LlmResponse`s.
        let _ = &self.api_key;
        let response = LlmResponse::new(Content::new("model").with_text("hello"));
        Ok(Box::pin(futures::stream::iter([Ok(response)])))
    }
}
```

Override `schema_adapter()` when the provider needs provider-specific tool schema
normalization.

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
use std::sync::Arc;

use adk_core::{Agent, Content, Event, EventStream, InvocationContext, Result};
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

    // Required: leaf agents return an empty slice.
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        let mut event = Event::new(ctx.invocation_id());
        event.author = self.name.clone();
        event.llm_response.content = Some(Content::new("model").with_text("done"));
        Ok(Box::pin(futures::stream::iter([Ok(event)])))
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
   adk_telemetry::init_telemetry("my-agent")?;
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

**Previous**: [← Access Control](../security/access-control.md)

**Questions?** Open an issue on [GitHub](https://github.com/zavora-ai/adk-rust/issues).
