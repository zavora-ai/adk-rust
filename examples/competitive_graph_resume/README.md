# Graph Durable Resume Example

Validation example for resume-from-checkpoint in `adk-graph`.

## What This Shows

- **`MemoryCheckpointer`** — save/load round-trip for graph state
- **Durable resume** — restoring state, pending nodes, and step from the last checkpoint
- **`StreamEvent::Resumed`** — emitted when execution restarts from a checkpoint

## Prerequisites

- **Rust 1.95+** (edition 2024)
- No API key required

## Run

```bash
cargo run --manifest-path examples/competitive_graph_resume/Cargo.toml
```
