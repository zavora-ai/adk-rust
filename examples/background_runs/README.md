# Background Runs Example

Exercises the Background Runs REST API from `adk-server`: submit a run, poll its status, and cancel it.

## What This Shows

- **`BackgroundState` / `BackgroundRunner`** — the shared state and executor behind the REST surface
- **`POST/GET/DELETE /runs`** — submit, inspect, and cancel asynchronous runs
- **Timeout and retry configuration** — per-run limits carried on the run record

## Prerequisites

- **Rust 1.95+** (edition 2024)
- Built with the adk-server `background` feature (already enabled in this example's manifest)

## Run

```bash
cargo run --manifest-path examples/background_runs/Cargo.toml
```
