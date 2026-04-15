# adk-telemetry

OpenTelemetry integration for Rust Agent Development Kit (ADK-Rust) agent observability.

[![Crates.io](https://img.shields.io/crates/v/adk-telemetry.svg)](https://crates.io/crates/adk-telemetry)
[![Documentation](https://docs.rs/adk-telemetry/badge.svg)](https://docs.rs/adk-telemetry)
[![License](https://img.shields.io/crates/l/adk-telemetry.svg)](LICENSE)

## Overview

`adk-telemetry` provides observability infrastructure for the Rust Agent Development Kit ([ADK-Rust](https://github.com/zavora-ai/adk-rust)), built on OpenTelemetry 0.31 and tracing-opentelemetry 0.32:

- **Tracing** - Distributed tracing with OpenTelemetry 0.31
- **Logging** - Structured logging with tracing-subscriber
- **Metrics** - Performance metrics export via OTLP (tonic 0.12 / gRPC)
- **Span Context** - Propagation across agent boundaries

## Installation

```toml
[dependencies]
adk-telemetry = "0.6.0"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "0.6.0", features = ["telemetry"] }
```

## Quick Start

```rust
use adk_telemetry::init_telemetry;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize with service name
    init_telemetry("my-agent")?;

    // Your agent code here...
    Ok(())
}
```

## Configuration

Set the `RUST_LOG` environment variable:

```bash
# Debug logging for ADK
RUST_LOG=adk=debug cargo run

# Trace level for specific modules
RUST_LOG=adk_agent=trace,adk_model=debug cargo run
```

## OpenTelemetry Export

Configure OTLP export for distributed tracing:

```rust
use adk_telemetry::init_with_otlp;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_with_otlp("my-agent", "http://localhost:4317")?;
    
    // Your agent code here...
    Ok(())
}
```

## Available Functions

| Function | Description |
|----------|-------------|
| `init_telemetry(service_name)` | Basic console logging |
| `init_with_otlp(service_name, endpoint)` | OTLP export to collectors |
| `init_with_adk_exporter(service_name)` | ADK-style span exporter |
| `shutdown_telemetry()` | Flush and shutdown |

## Span Helpers

Pre-configured spans for instrumenting ADK operations:

| Function | Description |
|----------|-------------|
| `agent_run_span(name, invocation_id)` | Agent execution span |
| `model_call_span(model_name)` | Model API call span |
| `llm_generate_span(provider, model, stream)` | LLM generation span with `gen_ai.usage.*` fields |
| `tool_execute_span(tool_name)` | Tool execution span |
| `callback_span(callback_type)` | Callback execution span |
| `record_llm_usage(&usage)` | Record token counts on the current span |

### Token Usage Tracking

`llm_generate_span` pre-declares OpenTelemetry GenAI semantic convention fields. After receiving a response, call `record_llm_usage` to populate them:

```rust
use adk_telemetry::{llm_generate_span, record_llm_usage, LlmUsage};

let span = llm_generate_span("openai", "gpt-5-mini", true);
let _enter = span.enter();

// After receiving the LLM response:
record_llm_usage(&LlmUsage {
    input_tokens: 100,
    output_tokens: 50,
    total_tokens: 150,
    cache_read_tokens: Some(80),
    ..Default::default()
});
```

Recorded fields: `gen_ai.usage.input_tokens`, `output_tokens`, `total_tokens`, `cache_read_tokens`, `cache_creation_tokens`, `thinking_tokens`, `audio_input_tokens`, `audio_output_tokens`.

## Re-exports

Convenience re-exports from `tracing`:

```rust
use adk_telemetry::{info, debug, warn, error, trace, instrument, Span};
```

## Features

- Zero-config defaults with sensible logging
- OpenTelemetry 0.31 compatible span export
- OTLP export via `tonic 0.12` (gRPC), aligned with `adk-server`'s `hyper 1.x` / `http 1.x` stack
- Automatic context propagation
- JSON or pretty-print log formats

### OpenTelemetry Dependency Versions

| Crate | Version |
|-------|---------|
| `opentelemetry` | 0.31 |
| `opentelemetry_sdk` | 0.31 |
| `opentelemetry-otlp` | 0.31 |
| `tracing-opentelemetry` | 0.32 |

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-core](https://crates.io/crates/adk-core) - Core traits and types

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
