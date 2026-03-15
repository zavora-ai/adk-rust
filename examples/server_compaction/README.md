# Server Compaction Configuration Example

Demonstrates configuring context compaction through `ServerConfig` for long-running agent sessions.

## Features

- **`ServerConfig::with_compaction()`** — configure compaction at the server level
- **`EventsCompactionConfig`** — set compaction interval, overlap size, and summarizer
- **Automatic forwarding** — config flows through RuntimeController and A2A to RunnerConfig
- **Backward compatibility** — omitting compaction preserves current behavior

## How Compaction Works

When enabled, the runner automatically summarizes older events after `compaction_interval` events accumulate. The `overlap_size` controls how many recent events are kept alongside the summary for context continuity.

## Running

```bash
cargo run --example server_compaction
```
