# Telemetry SQLite Export Example

Traces an agent run end-to-end into a local SQLite file — no OTLP collector or backend to deploy.

## What This Shows

- **`AdkSpanLayer`** — tracing layer that forwards spans to a `SpanSink`
- **SQLite span sink** — spans written to a local file for offline inspection
- **Zero-infrastructure tracing** — query the run with plain SQL afterwards

## Prerequisites

- **Rust 1.95+** (edition 2024)
- **`GOOGLE_API_KEY`** environment variable set
- Built with the adk-telemetry `sqlite` feature (already enabled in this example's manifest)

## Run

```bash
cargo run --manifest-path examples/telemetry_sqlite_export/Cargo.toml
```
