# Functional Workflow Example

Demonstrates the Functional API from `adk-graph`: write a workflow as async functions instead of assembling a graph.

## What This Shows

- **`TaskContext`** — the handle passed to each `#[task]`
- **`ReducedValue` / `UntrackedValue` / `MessagesValue`** — typed state reducers
- **`StateSchemaValidator`** — type expectations enforced on state and task output
- **`ExecutionLog`** — the per-run record of task execution

## Prerequisites

- **Rust 1.95+** (edition 2024)
- Built with the adk-graph `functional` feature (already enabled in this example's manifest)

## Run

```bash
cargo run --manifest-path examples/functional_workflow/Cargo.toml
```
