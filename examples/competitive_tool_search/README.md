# Tool Search and Interruption Detection Example

Validation example for `ToolSearchConfig` filtering and realtime interruption modes.

## What This Shows

- **`ToolSearchConfig`** — regex-based tool filtering for the Anthropic provider
- **`InterruptionDetection`** — `Manual` versus `Automatic` VAD-based interruption

## Prerequisites

- **Rust 1.95+** (edition 2024)
- No API key required

## Run

```bash
cargo run --manifest-path examples/competitive_tool_search/Cargo.toml
```
