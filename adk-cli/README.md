# adk-cli

Command-line launcher for Rust Agent Development Kit (ADK-Rust) agents.

[![Crates.io](https://img.shields.io/crates/v/adk-cli.svg)](https://crates.io/crates/adk-cli)
[![Documentation](https://docs.rs/adk-cli/badge.svg)](https://docs.rs/adk-cli)
[![License](https://img.shields.io/crates/l/adk-cli.svg)](LICENSE)

## Overview

`adk-cli` provides command-line tools for the Rust Agent Development Kit ([ADK-Rust](https://github.com/zavora-ai/adk-rust)):

- **Launcher** - Interactive REPL for agent conversations
- **Server Mode** - HTTP server with web UI
- **Session Management** - Automatic session handling
- **Telemetry** - Integrated logging and tracing

## Installation

```toml
[dependencies]
adk-cli = "0.3.1"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "0.3.1", features = ["cli"] }
```

## Quick Start

### Interactive Mode

```rust
use adk_cli::Launcher;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let agent = create_your_agent()?;

    // Start interactive REPL
    Launcher::new(Arc::new(agent))
        .run()
        .await?;

    Ok(())
}
```

### Server Mode

```bash
# Run with serve subcommand
cargo run -- serve --port 8080

# Open http://localhost:8080 for web UI
```

### Custom Configuration

```rust
use adk_cli::Launcher;
use adk_artifact::InMemoryArtifactService;
use adk_core::StreamingMode;
use std::sync::Arc;

Launcher::new(Arc::new(agent))
    .app_name("my_app")
    .with_artifact_service(Arc::new(InMemoryArtifactService::new()))
    .with_streaming_mode(StreamingMode::SSE)
    .run()
    .await?;
```

## CLI Commands

When running in interactive mode:

| Command | Description |
|---------|-------------|
| Type message | Send to agent |
| `/quit` or `/exit` | Exit REPL |
| `/clear` | Clear conversation |
| Ctrl+C | Interrupt |

## Environment Variables

```bash
# Logging level
RUST_LOG=info

# API key (for Gemini)
GOOGLE_API_KEY=your-key
```

## Features

- Colored output with streaming
- History support (arrow keys)
- Graceful shutdown
- Error recovery

## Binary Installation

The `adk` binary is also available:

```bash
cargo install adk-cli

# Run your agent
adk --help
```

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-server](https://crates.io/crates/adk-server) - HTTP server
- [adk-runner](https://crates.io/crates/adk-runner) - Execution runtime

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
