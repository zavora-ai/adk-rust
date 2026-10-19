# adk-rust-macros

Procedural macros for ADK-Rust: `#[tool]` for tool definitions, and `#[entrypoint]` / `#[task]` for `adk-graph` functional workflows.

[![Crates.io](https://img.shields.io/crates/v/adk-rust-macros.svg)](https://crates.io/crates/adk-rust-macros)
[![Documentation](https://docs.rs/adk-rust-macros/badge.svg)](https://docs.rs/adk-rust-macros)
[![License](https://img.shields.io/crates/l/adk-rust-macros.svg)](../LICENSE)

## Overview

| Macro | Generates | Used with |
|-------|-----------|-----------|
| `#[tool]` | A zero-sized struct implementing `adk_core::Tool` from an async function | `adk-tool` |
| `#[entrypoint]` | A workflow agent struct with `new(checkpointer)` and `invoke(state, config)` | `adk-graph` (`functional` feature) |
| `#[task]` | A checkpointed wrapper `__task_<name>` with optional retry | `adk-graph` (`functional` feature) |

## Installation

The generated code refers to `adk_tool`, so `#[tool]` needs `adk-tool` beside it. `adk-tool` re-exports the macro as `adk_tool::tool`.

```toml
[dependencies]
adk-tool = "3.0.0"
schemars = "1.0"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
```

## Quick Start

```rust
use std::sync::Arc;

use adk_tool::{AdkError, Tool, tool};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize, JsonSchema)]
struct AddArgs {
    /// First number
    a: i64,
    /// Second number
    b: i64,
}

/// Add two numbers together.
#[tool]
async fn add(args: AddArgs) -> Result<Value, AdkError> {
    Ok(json!(args.a + args.b))
}

fn main() {
    // `#[tool]` generates `pub struct Add` implementing `Tool`.
    let tool: Arc<dyn Tool> = Arc::new(Add);
    assert_eq!(tool.name(), "add");
    assert_eq!(tool.description(), "Add two numbers together.");
}
```

Register the struct on an agent with `LlmAgentBuilder::tool(Arc::new(Add))`.

## Tool Attributes

```rust
use adk_tool::{AdkError, tool};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize, JsonSchema)]
struct LookupArgs {
    key: String,
}

#[derive(Deserialize, JsonSchema)]
struct StatusArgs {
    ticket: String,
    status: String,
}

/// Look up cached data — no side effects, safe for parallel dispatch.
#[tool(read_only, concurrency_safe)]
async fn cache_lookup(args: LookupArgs) -> Result<Value, AdkError> {
    Ok(json!({ "key": args.key, "result": "cached" }))
}

/// Set a ticket's status; setting the same status twice changes nothing.
#[tool(idempotent)]
async fn set_ticket_status(args: StatusArgs) -> Result<Value, AdkError> {
    Ok(json!({ "ticket": args.ticket, "status": args.status }))
}

/// Start a long-running background report.
#[tool(long_running)]
async fn generate_report() -> Result<Value, AdkError> {
    Ok(json!({ "task_id": "abc123", "status": "processing" }))
}

fn main() {}
```

| Attribute | Effect |
|-----------|--------|
| `read_only` | `is_read_only()` → `true`, so `effect()` → `ToolEffect::ReadOnly`; one of the two signals `ToolExecutionStrategy::Auto` needs for concurrent dispatch |
| `idempotent` | `effect()` → `ToolEffect::Idempotent`, so a retry budget may retry the call after a retryable error; ignored beside `read_only` |
| `concurrency_safe` | `is_concurrency_safe()` → `true`; the other signal `Auto` needs |
| `long_running` | `is_long_running()` → `true`; the model event lists the call in `long_running_tool_ids` |
| `lenient_args` | Turns off argument validation: serde alone decodes the arguments, unknown keys are dropped, and schema bounds are advisory |

Plain `#[tool]` keeps the defaults: every flag `false` and `effect()` → `ToolEffect::NonIdempotent`, so the call runs once and is not retried.

## Argument Validation

Before the function runs, the generated `execute()` checks the arguments against the argument type's schema, with `additionalProperties: false` on every object that declares all of its fields (for the check only; `parameters_schema()` leaves it out, since Gemini rejects it). A key the schema does not name, a value outside a `#[schemars(range(...))]`, `length(...)`, or `pattern(...)` bound, or a missing required field fails the call with an `ErrorCategory::InvalidInput` error with code `tool.invalid_arguments` (`adk_core::TOOL_INVALID_ARGUMENTS_CODE`) that lists each violation by JSON pointer. `LlmAgent` shows that message to the model so it can correct the call. `null` stays accepted for an `Option` field. A value serde cannot decode fails with the same code.

In both modes an integral float at an integer field, such as `3.0` for a `u32`, is read as the integer; some models write every number with a decimal point.

A schema narrower than what the type deserializes from — a `#[serde(alias)]` key, a custom `deserialize_with` — also rejects calls serde would accept. Keep one struct open with `#[schemars(extend("additionalProperties" = true))]`, or opt the tool out with `#[tool(lenient_args)]`.

## What the Macro Generates

| Input | Output |
|-------|--------|
| Function name `get_weather` | Tool name `"get_weather"` |
| Doc comment `/// Get the current weather.` | Tool description `"Get the current weather."`; without one, the function name with `_` replaced by spaces |
| Argument type `WeatherArgs` | `parameters_schema()` from `schemars::schema_for!(WeatherArgs)` |
| — | Struct `GetWeather` implementing `Tool` |

The generated struct has the function's visibility and implements:

- `name()` — the snake_case function name
- `description()` — the doc comment text
- `parameters_schema()` — the JSON schema of the argument type, or `None` without one
- `is_read_only()`, `is_concurrency_safe()`, `is_long_running()`, `effect()` — from the attributes
- `execute()` — validates the arguments, deserializes them, and calls the function

The original function stays callable.

## Usage Patterns

### Tool with Context Access

A parameter whose type names `ToolContext` receives the call's context:

```rust
use std::sync::Arc;

use adk_tool::{AdkError, ToolContext, tool};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize, JsonSchema)]
struct ReadStateArgs {
    /// The state key to read
    key: String,
}

/// Read a value from the session state.
#[tool(read_only)]
async fn read_state(ctx: Arc<dyn ToolContext>, args: ReadStateArgs) -> Result<Value, AdkError> {
    Ok(json!({ "key": args.key, "call_id": ctx.function_call_id() }))
}

fn main() {}
```

### Tool Without Arguments

```rust
use adk_tool::{AdkError, tool};
use serde_json::{Value, json};

/// Get the current server time.
#[tool(read_only)]
async fn get_time() -> Result<Value, AdkError> {
    Ok(json!({ "time": "2026-03-26T12:00:00Z" }))
}

fn main() {}
```

## Schema Cleaning

`parameters_schema()` returns the schema in the form sent to the model (`adk_tool::model_facing_schema`):

- Removes the root `$schema` and `title`
- Reduces nullable types: `{"type": ["string", "null"]}` → `{"type": "string"}`
- Replaces an `anyOf` of one schema and `null` with that schema

Validation uses the schema before this reduction, so a model may still send `null` for an optional field.

## Naming Convention

| Function | Generated Struct |
|----------|------------------|
| `get_weather` | `GetWeather` |
| `search_docs` | `SearchDocs` |
| `add` | `Add` |
| `send_email_notification` | `SendEmailNotification` |

## Requirements

- The function is `async`
- It takes at most one argument type besides an optional `Arc<dyn ToolContext>`; the type implements `serde::de::DeserializeOwned` and `schemars::JsonSchema`
- It returns `Result<serde_json::Value, adk_tool::AdkError>`
- The calling crate depends on `adk-tool`, `schemars`, and `serde_json`

## Functional Workflows

`#[entrypoint]` and `#[task]` write `adk-graph` workflows as async functions. Each `#[task]` generates `__task_<name>`, which records the call's result in the `ExecutionLog`, replays it on resume, and applies `retry(max_attempts = N, backoff = "Xs")` (linear backoff) or `rerun_on_resume`. Call the `__task_` wrapper from the entrypoint to get that behaviour. `#[entrypoint]` generates `<Name>Agent` with `new(Arc<dyn Checkpointer>)` and `invoke(State, ExecutionConfig)`; `invoke` resumes from the checkpoint `ExecutionConfig::resume_from` names and fails with `GraphError::CheckpointError` when the thread has no checkpoint with that id.

```toml
[dependencies]
adk-graph = { version = "3.0.0", features = ["functional"] }
adk-rust-macros = "3.0.0"
serde_json = "1.0"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

```rust
use std::sync::Arc;

use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer};
use adk_graph::error::Result;
use adk_graph::functional::TaskContext;
use adk_graph::node::ExecutionConfig;
use adk_graph::state::State;
use adk_rust_macros::{entrypoint, task};
use serde_json::{Value, json};

#[task(retry(max_attempts = 3, backoff = "1s"))]
async fn double(_ctx: &mut TaskContext, n: i64) -> Result<Value> {
    Ok(json!(n * 2))
}

#[entrypoint]
async fn double_twice(ctx: &mut TaskContext) -> Result<Value> {
    let first = __task_double(ctx, 1).await?;
    let second = __task_double(ctx, 2).await?;
    ctx.set("results", json!([first, second]));
    Ok(Value::Null)
}

#[tokio::main]
async fn main() -> Result<()> {
    let checkpointer = Arc::new(MemoryCheckpointer::new()) as Arc<dyn Checkpointer>;
    let state = DoubleTwiceAgent::new(checkpointer)
        .invoke(State::new(), ExecutionConfig::new("double-twice"))
        .await?;
    assert_eq!(state.get("results"), Some(&json!([2, 4])));
    Ok(())
}
```

## Changes in 3.0

| Change | Migration |
|--------|-----------|
| `#[tool]` validates arguments against the schema before the function runs | Opt out with `#[tool(lenient_args)]` or keep one struct open with `#[schemars(extend("additionalProperties" = true))]` |
| `#[tool(idempotent)]` declares `ToolEffect::Idempotent`; a tool without `read_only` or `idempotent` is no longer retried | Add `idempotent` where retries are safe |
| Argument decoding errors are `ErrorCategory::InvalidInput` with code `tool.invalid_arguments` | Match the new code |
| `#[entrypoint]` resumes from the checkpoint `resume_from` names | Pass the id `Checkpointer::load(thread_id)` returns to resume where a thread left off |

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
