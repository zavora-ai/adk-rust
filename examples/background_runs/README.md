# Background Runs Example

Exercises the Background Runs REST API from `adk-server`: submit a run, poll its status, and cancel it.

## What This Shows

- **`BackgroundState` / `WorkflowRegistry`** — an in-memory `DurableRunStore`, the registered workflows, and the embedded `DurableWorker` that claims and executes runs
- **`POST/GET/DELETE /runs`** — submit, inspect, and cancel asynchronous runs
- **Retry configuration** — `maxRetries` sets the run's attempt budget

## Prerequisites

- **Rust 1.95+** (edition 2024)
- Built with the adk-server `background` feature (already enabled in this example's manifest)

## Run

```bash
cargo run --manifest-path examples/background_runs/Cargo.toml
```
