# adk-runner

Agent execution runtime for ADK.

[![Crates.io](https://img.shields.io/crates/v/adk-runner.svg)](https://crates.io/crates/adk-runner)
[![Documentation](https://docs.rs/adk-runner/badge.svg)](https://docs.rs/adk-runner)
[![License](https://img.shields.io/crates/l/adk-runner.svg)](LICENSE)

## Overview

`adk-runner` provides the execution runtime for ADK agents:

- **Runner** - Manages agent execution with full context
- **Session Integration** - Automatic session creation and state management
- **Memory Injection** - Retrieves and injects relevant memories
- **Artifact Handling** - Manages binary artifacts during execution
- **Event Streaming** - Streams agent events with backpressure

## Installation

```toml
[dependencies]
adk-runner = "0.1"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "0.1", features = ["runner"] }
```

## Quick Start

```rust
use adk_runner::{Runner, RunnerConfig};
use adk_session::InMemorySessionService;
use adk_artifact::InMemoryArtifactService;
use std::sync::Arc;

// Create services
let sessions = Arc::new(InMemorySessionService::new());
let artifacts = Arc::new(InMemoryArtifactService::new());

// Configure runner
let config = RunnerConfig {
    app_name: "my_app".to_string(),
    session_service: sessions,
    artifact_service: Some(artifacts),
    memory_service: None,
};

// Create runner
let runner = Runner::new(config);

// Run agent
let mut stream = runner.run(
    agent,
    "user_123",
    Some("session_456"),
    Content::text("Hello!"),
).await?;

// Process events
while let Some(event) = stream.next().await {
    // Handle event...
}
```

## Runner vs Direct Agent Execution

| Feature | Direct `agent.run()` | `Runner` |
|---------|---------------------|----------|
| Session management | Manual | Automatic |
| Memory injection | Manual | Automatic |
| Artifact storage | Manual | Automatic |
| State persistence | Manual | Automatic |

Use `Runner` for production; direct execution for testing.

## Features

- Automatic context creation
- Session restore and persistence
- Configurable memory retrieval
- Event history management
- Concurrent-safe execution

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-core](https://crates.io/crates/adk-core) - Core traits
- [adk-session](https://crates.io/crates/adk-session) - Session storage
- [adk-cli](https://crates.io/crates/adk-cli) - CLI using runner

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://github.com/zavora-ai/adk-rust) framework for building AI agents in Rust.
