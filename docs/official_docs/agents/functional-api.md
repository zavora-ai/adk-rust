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
adk-graph = { version = "2.3.0", features = ["functional"] }
adk-rust-macros = "2.3.0"
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

### Task Identity

`#[task]` generates a wrapper named `__task_<name>` that records each **call** in
the `ExecutionLog` under its own key, built by `adk_graph::functional::task_call_id`:

| Part | Meaning |
|------|---------|
| `<name>` | The task function's name |
| `#<ordinal>` | How many calls of this task preceded this one in the run, from `0` |
| `:<arg_hash>` | A stable 64-bit FNV-1a hash of the canonical JSON of the arguments that implement `Serialize`; omitted when none does |

```rust,ignore
use adk_graph::error::Result;
use adk_graph::functional::TaskContext;
use adk_rust_macros::{entrypoint, task};
use serde_json::{Value, json};

#[task]
async fn process(_ctx: &mut TaskContext, item: String) -> Result<Value> {
    Ok(json!(format!("processed {item}")))
}

#[entrypoint]
async fn process_all(ctx: &mut TaskContext) -> Result<Value> {
    let mut outputs = Vec::new();
    for item in ["a", "b", "c"] {
        // Logged as process#0:…, process#1:…, process#2:…
        outputs.push(__task_process(ctx, item.to_string()).await?);
    }
    ctx.set("outputs", json!(outputs));
    Ok(Value::Null)
}
```

A resumed run replays the entrypoint from the top. Each call reaches the same
ordinal it had in the run being resumed, so it returns its own recorded result.
A call whose arguments differ from the recorded call at that position gets a new
key and runs again rather than returning another call's result.

> **Note:** Before this scheme a task was keyed by its name alone, so a task called
> in a loop returned the first call's result for every later call. An execution log
> written under the old keys does not match the new ones, so a workflow resumed
> across the upgrade runs its tasks again.

### Execution Guarantees

A task is **at-least-once**. Its body runs, then its completion is checkpointed;
a crash between the two runs the task again on resume. Make a task with external
side effects idempotent. `rerun_on_resume` opts a task out of replay entirely, so
it runs on every resume.

## Background Runs

The `background` feature in `adk-server` adds REST endpoints for async workflow execution.

### Endpoints

| Method | Path | Description |
|--------|------|-------------|
| POST | `/runs` | Submit a background run |
| GET | `/runs/{run_id}` | Poll run status |
| DELETE | `/runs/{run_id}` | Cancel a run |

### Usage

A run names a `workflowId`. Something has to turn that name into work, so register a
`WorkflowExecutor` — without one, a submitted run **fails** rather than reporting
completion:

```rust
use adk_server::background::{
    BackgroundState, WorkflowRegistry, background_runs_router_with_state,
};
use serde_json::json;
use std::sync::Arc;

let registry = WorkflowRegistry::new().register("summarize", |input, cancel| async move {
    if cancel.is_cancelled() {
        return Err("cancelled before starting".to_string());
    }
    Ok(json!({ "summary": "…", "of": input.get("document") }))
});

let state = BackgroundState::new().with_executor(Arc::new(registry));
let app = axum::Router::new().merge(background_runs_router_with_state(state));
```

> **Important:** `background_runs_router` and `background_runs_router_with_state` carry no
> authentication. Anyone who reaches a standalone mount can submit and cancel runs. Mount
> them behind your own authentication layer, or let `ServerBuilder` mount them under `/api`
> behind the server's auth middleware:
>
> ```rust
> use adk_server::ServerBuilder;
>
> // `config` is a ServerConfig with `.with_request_context(extractor)` applied.
> let app = ServerBuilder::new(config)
>     .with_background_runs(state)
>     .build(); // serves /api/runs and /api/runs/{run_id}
> ```

Submitting an unregistered `workflowId` returns **404** rather than queuing a run that
can never execute. Implement `WorkflowExecutor` directly to bridge to `adk-graph`, the
functional API, or your own dispatcher; return `Err` to fail the run, which is what makes
the retry budget meaningful.

### Status Lifecycle

```
queued → running → completed
                 → failed (retries if configured)
                 → cancelled (via DELETE)
```

Retry re-executes the workflow **from the beginning** with the original input. It is not
checkpoint-aware, so a workflow with side effects should be idempotent or guard its own
progress.

> **Important:** run records live in an in-memory store. A process restart loses every
> record, including in-flight runs. Treat these endpoints as an async execution surface,
> not a durable job queue.

## Cron Scheduling

The `background` feature also includes cron job management.

### Endpoints

| Method | Path | Description |
|--------|------|-------------|
| POST | `/cron` | Create a cron job |
| GET | `/cron` | List all jobs |
| GET | `/cron/{job_id}` | Get one job |
| PATCH | `/cron/{job_id}` | Pause/resume |
| DELETE | `/cron/{job_id}` | Delete a job |

### Concurrency Policies

- **skip**: Skip the occurrence if a previous run is still active (default)
- **allow**: Permit concurrent executions
- **queue**: Queue the occurrence and run it when the active run finishes

Each schedule occurrence is claimed once, so an occurrence that arrives while a run is
active produces exactly one queued run rather than one per scheduler poll. Queued runs
drain in order, and each is monitored the same way a directly triggered run is, so the
active count returns to zero even if a run fails or is cancelled.

A job's queue holds at most 100 runs by default (`DEFAULT_MAX_QUEUE_DEPTH`). An occurrence
that arrives while the queue is full is skipped with a warning. Set the depth with
`CronState::with_max_queue_depth` before cloning the state into the router and scheduler.

### Pause and Resume

Resuming a paused job (`PATCH` with `"status": "active"`) skips every occurrence that fell
inside the pause: the next run is the first occurrence after the resume instant, so a
per-minute job paused for a day fires once at the next minute rather than replaying its
missed occurrences. `lastExecution` reports the scheduling cursor — the latest claimed
occurrence, or the resume instant after a pause.

`executionCount` counts the runs the scheduler started, including runs taken off the
queue. Occurrences dropped by `skip` or by a full queue are not counted.

Cron jobs use the same executor as background runs — a job whose `workflowId` is not
registered will fail its runs.

### Usage

```rust
use adk_server::background::{BackgroundState, CronState, cron_jobs_router_with_state, start_cron_scheduler};

let bg_state = BackgroundState::new();
let cron_state = CronState::new(bg_state);
let app = axum::Router::new().merge(cron_jobs_router_with_state(cron_state.clone()));

// Start the background scheduler
start_cron_scheduler(cron_state);
```

> **Important:** `cron_jobs_router` and `cron_jobs_router_with_state` carry no
> authentication. Anyone who reaches a standalone mount can schedule recurring work. Mount
> them behind your own authentication layer, or use
> `ServerBuilder::with_cron_jobs(cron_state)`, which serves `/api/cron` behind the server's
> auth middleware. The builder does not start the scheduler — call `start_cron_scheduler`
> with a clone of the same state.

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

## Interrupt and typed resume

`TaskContext::interrupt<T>` suspends the workflow and, on a later run, returns the value supplied
for that site.

Each interrupt site gets a **continuation key** from its position in the run — `interrupt-1`,
`interrupt-2`, and so on — so a replayed workflow reaches the same interrupts in the same order
and finds the value it was given.

```rust,ignore
// First run: no value supplied, so the workflow suspends.
let error = ctx.interrupt::<Approval>("approve the refund").await.unwrap_err();
// workflow suspended at interrupt 'interrupt-1': approve the refund

// Second run: supply the value under that key.
let ctx = ctx.with_resume_values(HashMap::from([
    ("interrupt-1".to_string(), serde_json::json!({ "approved": true, "approver": "alice" })),
]));
let approval: Approval = ctx.interrupt("approve the refund").await?;
```

The key is also written to the interrupt checkpoint under `continuation_key`.

| Situation | Result |
|-----------|--------|
| No value for the key | `FunctionalError::Suspended { continuation_key, message }` |
| A value that deserializes into `T` | `Ok(value)` |
| A value that does not deserialize into `T` | `FunctionalError::InterruptTypeMismatch` naming the site |

> **Important:** `interrupt` previously returned `InterruptTypeMismatch` with "workflow
> interrupted" on **every** call, and nothing outside the method consumed a resume value. A caller
> could not tell "needs input" from "your value was the wrong type", had no key to supply a value
> under, and never received a typed value at the call site.

> **Note:** `From<FunctionalError> for GraphError` flattens to `GraphError::Other(String)`, so a
> caller matching structurally should match on `FunctionalError` before conversion.
