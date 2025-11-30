# adk-telemetry

OpenTelemetry integration for ADK agent observability.

[![Crates.io](https://img.shields.io/crates/v/adk-telemetry.svg)](https://crates.io/crates/adk-telemetry)
[![Documentation](https://docs.rs/adk-telemetry/badge.svg)](https://docs.rs/adk-telemetry)
[![License](https://img.shields.io/crates/l/adk-telemetry.svg)](LICENSE)

## Overview

`adk-telemetry` provides observability infrastructure for ADK agents, including:

- **Tracing** - Distributed tracing with OpenTelemetry
- **Logging** - Structured logging with tracing-subscriber
- **Metrics** - Performance metrics export
- **Span Context** - Propagation across agent boundaries

## Installation

```toml
[dependencies]
adk-telemetry = "0.1"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "0.1", features = ["telemetry"] }
```

## Quick Start

```rust
use adk_telemetry::init_tracing;

fn main() {
    // Initialize with environment filter
    init_tracing();

    // Your agent code here...
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
use adk_telemetry::TelemetryConfig;

let config = TelemetryConfig::builder()
    .service_name("my-agent")
    .otlp_endpoint("http://localhost:4317")
    .build();

init_with_config(config);
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

This crate is part of the [ADK-Rust](https://github.com/anthropics/adk-rust) framework for building AI agents in Rust.
