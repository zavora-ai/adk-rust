# Functional API

Write agent workflows as normal async Rust functions with automatic checkpointing, typed state reducers, and interrupt/resume support.

## Overview

The Functional API (`functional` feature in `adk-graph`) provides a higher-level alternative to explicit graph/node/edge construction. Instead of building a StateGraph manually, you annotate functions with `#[entrypoint]` and `#[task]` macros and use standard Rust control flow.

**Key Benefits:**
- Write workflows as normal async Rust — no graph DSL required
- Automatic checkpointing after each task for crash recovery
- Standard Rust control flow (if/else, for, loop, match)
- Typed state containers with persistence guarantees
- Compatible with existing StreamEvent and Checkpointer infrastructure

## Getting Started

```toml
[dependencies]
adk-graph = { version = "1.0.0", features = ["functional"] }
adk-rust-macros = "1.0.0"
```

## Core Types

### TaskContext

The runtime context passed to all workflow functions. Provides access to state, checkpointing, interrupts, and streaming.

```rust
use adk_graph::functional::TaskContext;

// Read state
let count: Option<i64> = ctx.get("counter");

// Write state (uses configured reducer)
ctx.set("counter", serde_json::json!(count.unwrap_or(0) + 1));

// Emit progress events
ctx.emit(StreamEvent::custom("my_task", "progress", json!({"pct": 50})));

// Interrupt for human-in-the-loop
let approval: bool = ctx.interrupt("Please approve").await?;
```

### ReducedValue<T>

Append-only state container persisted across checkpoints. Values accumulate and are never overwritten.

```rust
use adk_graph::functional::ReducedValue;

let mut results: ReducedValue<String> = ReducedValue::new();
results.push("step 1 output".to_string());
results.push("step 2 output".to_string());
assert_eq!(results.len(), 2);
assert_eq!(&results[0], "step 1 output");
```

### UntrackedValue<T>

Transient runtime values excluded from checkpoint persistence. Resets to default on resume.

```rust
use adk_graph::functional::UntrackedValue;

let mut temp: UntrackedValue<Vec<u8>> = UntrackedValue::new();
temp.set(vec![1, 2, 3]);
// After checkpoint restore: temp.get() == &[]
```

### MessagesValue

Chat message container with automatic deduplication based on message IDs.

```rust
use adk_graph::functional::{MessagesValue, ChatMessage, MessageRole};

let mut messages = MessagesValue::new();
messages.push(ChatMessage {
    id: "msg-1".to_string(),
    role: MessageRole::User,
    content: "Hello".to_string(),
    metadata: None,
});
// Pushing same ID replaces the message (upsert)
```

### StateSchemaValidator

Validates state types at workflow boundaries — catches mismatches early.

```rust
use adk_graph::functional::{StateSchemaValidator, ExpectedType};
use adk_graph::state::StateSchema;

let validator = StateSchemaValidator::new(schema)
    .expect_type("counter", ExpectedType::Number)
    .expect_type("status", ExpectedType::String)
    .require_field("status");

validator.validate_state(&state)?; // Fails with descriptive error
```

### ExecutionLog

Tracks task completion for resume-skip behavior. Completed tasks are skipped on workflow resume.

```rust
use adk_graph::functional::ExecutionLog;

let mut log = ExecutionLog::new();
log.record_start("fetch");
log.record_completion("fetch", json!({"data": [1,2,3]}));

// On resume: skip completed tasks
if log.is_completed("fetch") {
    let cached = log.get_result("fetch"); // Returns cached result
}
```

## Background Runs

The `background` feature in `adk-server` adds REST endpoints for async workflow execution.

### Endpoints

| Method | Path | Description |
|--------|------|-------------|
| POST | `/runs` | Submit a background run |
| GET | `/runs/{run_id}` | Poll run status |
| DELETE | `/runs/{run_id}` | Cancel a run |

### Usage

```rust
use adk_server::background::{BackgroundState, background_runs_router_with_state};

let state = BackgroundState::new();
let app = axum::Router::new().merge(background_runs_router_with_state(state));
```

### Status Lifecycle

```
queued → running → completed
                 → failed (retries if configured)
                 → cancelled (via DELETE)
```

## Cron Scheduling

The `background` feature also includes cron job management.

### Endpoints

| Method | Path | Description |
|--------|------|-------------|
| POST | `/cron` | Create a cron job |
| GET | `/cron` | List all jobs |
| PATCH | `/cron/{job_id}` | Pause/resume |
| DELETE | `/cron/{job_id}` | Delete a job |

### Concurrency Policies

- **skip**: Skip execution if previous run still active (default)
- **allow**: Permit concurrent executions
- **queue**: Queue new execution until current completes

### Usage

```rust
use adk_server::background::{BackgroundState, CronState, cron_jobs_router_with_state, start_cron_scheduler};

let bg_state = BackgroundState::new();
let cron_state = CronState::new(bg_state);
let app = axum::Router::new().merge(cron_jobs_router_with_state(cron_state.clone()));

// Start the background scheduler
start_cron_scheduler(cron_state);
```

## Examples

```bash
# Functional API (TaskContext, ReducedValue, MessagesValue, etc.)
cargo run --manifest-path examples/functional_workflow/Cargo.toml

# Background Runs (REST API with Axum)
cargo run --manifest-path examples/background_runs/Cargo.toml

# Cron Scheduling (full lifecycle demo)
cargo run --manifest-path examples/cron_scheduling/Cargo.toml
```

## Feature Flags

| Feature | Crate | Adds |
|---------|-------|------|
| `functional` | `adk-graph` | TaskContext, typed reducers, schema validation, proc macros |
| `background` | `adk-server` | Background run endpoints, cron scheduling, scheduler loop |

---

**Previous**: [← Graph Agents](./graph-agents.md) | **Next**: [Realtime Agents →](./realtime-agents.md)
