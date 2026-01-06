# adk-runner

Agent execution runtime for ADK-Rust.

[![Crates.io](https://img.shields.io/crates/v/adk-runner.svg)](https://crates.io/crates/adk-runner)
[![Documentation](https://docs.rs/adk-runner/badge.svg)](https://docs.rs/adk-runner)
[![License](https://img.shields.io/crates/l/adk-runner.svg)](LICENSE)

## Overview

`adk-runner` provides the execution runtime for [ADK-Rust](https://github.com/zavora-ai/adk-rust):

- **Runner** - Manages agent execution with full context
- **Session Integration** - Automatic session creation and state management
- **Memory Injection** - Retrieves and injects relevant memories
- **Artifact Handling** - Manages binary artifacts during execution
- **Event Streaming** - Streams agent events with state propagation
- **Agent Transfer** - Automatic handling of agent-to-agent transfers

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
use adk_core::Content;
use std::sync::Arc;

// Create services
let sessions = Arc::new(InMemorySessionService::new());
let artifacts = Arc::new(InMemoryArtifactService::new());

// Configure runner with agent
let config = RunnerConfig {
    app_name: "my_app".to_string(),
    agent: my_agent,  // Arc<dyn Agent>
    session_service: sessions,
    artifact_service: Some(artifacts),
    memory_service: None,
    run_config: None,  // Uses default SSE streaming
};

// Create runner
let runner = Runner::new(config)?;

// Run agent for a user/session
let mut stream = runner.run(
    "user_123".to_string(),
    "session_456".to_string(),
    Content::new("user").with_text("Hello!"),
).await?;

// Process events
use futures::StreamExt;
while let Some(event) = stream.next().await {
    match event {
        Ok(e) => println!("Event: {:?}", e.content()),
        Err(e) => eprintln!("Error: {}", e),
    }
}
```

## RunnerConfig

| Field | Type | Description |
|-------|------|-------------|
| `app_name` | `String` | Application identifier |
| `agent` | `Arc<dyn Agent>` | Root agent to execute |
| `session_service` | `Arc<dyn SessionService>` | Session storage backend |
| `artifact_service` | `Option<Arc<dyn ArtifactService>>` | Optional artifact storage |
| `memory_service` | `Option<Arc<dyn Memory>>` | Optional memory/RAG service |
| `run_config` | `Option<RunConfig>` | Streaming mode config |

## Runner vs Direct Agent Execution

| Feature | Direct `agent.run()` | `Runner` |
|---------|---------------------|----------|
| Session management | Manual | Automatic |
| Memory injection | Manual | Automatic |
| Artifact storage | Manual | Automatic |
| State persistence | Manual | Automatic |
| Agent transfers | Manual | Automatic |
| Event history | Manual | Automatic |

Use `Runner` for production; direct execution for testing.

## Agent Transfers

Runner automatically handles agent-to-agent transfers:

```rust
// When an agent sets transfer_to_agent in EventActions,
// Runner automatically:
// 1. Finds the target agent in the agent tree
// 2. Creates a new invocation context
// 3. Preserves session state across the transfer
// 4. Continues streaming events from the new agent
```

## State Propagation

Runner applies state changes immediately:

```rust
// When an agent emits an event with state_delta,
// Runner applies it to the mutable session so
// downstream agents can read the updated state.
```

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-core](https://crates.io/crates/adk-core) - Core traits
- [adk-session](https://crates.io/crates/adk-session) - Session storage
- [adk-artifact](https://crates.io/crates/adk-artifact) - Artifact storage
- [adk-cli](https://crates.io/crates/adk-cli) - CLI using runner

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
