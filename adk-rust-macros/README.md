# adk-rust-macros

Proc macros for ADK-Rust — `#[tool]` attribute for zero-boilerplate tool registration.

## Overview

This crate provides the `#[tool]` attribute macro that turns an async function into a full `adk_core::Tool` implementation. No manual struct definitions, no trait boilerplate, no JSON schema wiring.

## Installation

```toml
[dependencies]
adk-rust-macros = "0.5.0"
adk-tool = "0.5.0"
schemars = "1.0"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
```

## Quick Start

```rust,ignore
use adk_rust_macros::tool;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
struct AddArgs {
    /// First number
    a: i64,
    /// Second number
    b: i64,
}

/// Add two numbers together.
#[tool]
async fn add(args: AddArgs) -> Result<serde_json::Value, adk_tool::AdkError> {
    Ok(serde_json::json!(args.a + args.b))
}

// Generated: pub struct Add; implements adk_core::Tool
// Register: agent_builder.tool(Arc::new(Add))
```

## Tool Metadata Attributes

Mark tools as read-only, concurrency-safe, or long-running directly in the macro:

```rust,ignore
/// Look up cached data — no side effects, safe for parallel dispatch.
#[tool(read_only, concurrency_safe)]
async fn cache_lookup(args: LookupArgs) -> Result<serde_json::Value, adk_tool::AdkError> {
    Ok(serde_json::json!({"result": "cached"}))
}

/// Start a long-running background report.
#[tool(long_running)]
async fn generate_report(args: ReportArgs) -> Result<serde_json::Value, adk_tool::AdkError> {
    Ok(serde_json::json!({"task_id": "abc123", "status": "processing"}))
}
```

| Attribute | Effect |
|-----------|--------|
| `read_only` | `is_read_only() → true` — included in concurrent batch under `Auto` strategy |
| `concurrency_safe` | `is_concurrency_safe() → true` — explicitly safe for parallel dispatch |
| `long_running` | `is_long_running() → true` — prevents LLM from re-calling a pending tool |

Plain `#[tool]` without attributes keeps the defaults (all `false`), so existing code is unaffected.

## What the Macro Generates

For a function `get_weather`, the macro generates:

| Input | Output |
|-------|--------|
| Function name `get_weather` | Tool name `"get_weather"` |
| Doc comment `/// Get the current weather.` | Tool description `"Get the current weather."` |
| Arg type `WeatherArgs` | JSON schema via `schemars::schema_for!(WeatherArgs)` |
| — | Struct `GetWeather` implementing `Tool` trait |

The generated struct is zero-sized and implements `adk_core::Tool` with:
- `name()` → snake_case function name
- `description()` → doc comment text
- `parameters_schema()` → JSON schema from the args type
- `is_read_only()` → `true` if `read_only` attribute is set
- `is_concurrency_safe()` → `true` if `concurrency_safe` attribute is set
- `is_long_running()` → `true` if `long_running` attribute is set
- `execute()` → deserializes args, calls your function

## Usage Patterns

### Simple tool (args only)

```rust,ignore
#[derive(Deserialize, JsonSchema)]
struct SearchArgs {
    /// The search query
    query: String,
    /// Maximum results to return
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize { 10 }

/// Search the knowledge base for documents matching a query.
#[tool]
async fn search_docs(args: SearchArgs) -> Result<serde_json::Value, adk_tool::AdkError> {
    // Your implementation here
    Ok(serde_json::json!({ "results": [], "query": args.query }))
}

// Use: Arc::new(SearchDocs)
```

### Tool with context access

```rust,ignore
use std::sync::Arc;
use adk_tool::ToolContext;

/// Read the current session state.
#[tool]
async fn read_state(
    ctx: Arc<dyn ToolContext>,
    args: ReadStateArgs,
) -> Result<serde_json::Value, adk_tool::AdkError> {
    // Access session, state, or other context
    Ok(serde_json::json!({ "key": args.key }))
}
```

### No-args tool

```rust,ignore
/// Get the current server time.
#[tool]
async fn get_time() -> Result<serde_json::Value, adk_tool::AdkError> {
    Ok(serde_json::json!({ "time": "2026-03-26T12:00:00Z" }))
}

// Use: Arc::new(GetTime)
```

## Schema Cleaning

The macro automatically cleans the generated JSON schema for LLM API compatibility:

- Strips `$schema` and `title` fields (rejected by Gemini)
- Simplifies nullable types: `{"type": ["string", "null"]}` → `{"type": "string"}`
- Unwraps `anyOf` wrappers for simple `Option<T>` fields

## Naming Convention

| Function | Generated Struct |
|----------|-----------------|
| `get_weather` | `GetWeather` |
| `search_docs` | `SearchDocs` |
| `add` | `Add` |
| `send_email_notification` | `SendEmailNotification` |

The function name (snake_case) becomes the tool name string. The struct name (PascalCase) is what you pass to `Arc::new()`.

## Requirements

- Function must be `async`
- Args type must implement `serde::Deserialize` and `schemars::JsonSchema`
- Return type must be `Result<serde_json::Value, adk_tool::AdkError>`
- Doc comments are used as the tool description (falls back to function name with underscores replaced by spaces)
- Optional attributes: `read_only`, `concurrency_safe`, `long_running` (all default to `false`)

## License

Apache-2.0
