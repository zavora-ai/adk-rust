# adk-telemetry

OpenTelemetry integration for Rust Agent Development Kit (ADK-Rust) agent observability.

[![Crates.io](https://img.shields.io/crates/v/adk-telemetry.svg)](https://crates.io/crates/adk-telemetry)
[![Documentation](https://docs.rs/adk-telemetry/badge.svg)](https://docs.rs/adk-telemetry)
[![License](https://img.shields.io/crates/l/adk-telemetry.svg)](LICENSE)

## Overview

`adk-telemetry` provides observability infrastructure for the Rust Agent Development Kit ([ADK-Rust](https://github.com/zavora-ai/adk-rust)), including:

- **Tracing** - Distributed tracing with OpenTelemetry
- **Logging** - Structured logging with tracing-subscriber
- **Metrics** - Performance metrics export
- **Span Context** - Propagation across agent boundaries

## Installation

```toml
[dependencies]
adk-telemetry = "0.3.0"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "0.3.0", features = ["telemetry"] }
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

## Re-exports

Convenience re-exports from `tracing`:

```rust
use adk_telemetry::{info, debug, warn, error, trace, instrument, Span};
```

## Features

- Zero-config defaults with sensible logging
- OpenTelemetry-compatible span export
- Automatic context propagation
- JSON or pretty-print log formats

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-core](https://crates.io/crates/adk-core) - Core traits and types

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
