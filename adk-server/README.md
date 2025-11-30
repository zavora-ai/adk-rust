# adk-server

HTTP server and A2A protocol for ADK agents.

[![Crates.io](https://img.shields.io/crates/v/adk-server.svg)](https://crates.io/crates/adk-server)
[![Documentation](https://docs.rs/adk-server/badge.svg)](https://docs.rs/adk-server)
[![License](https://img.shields.io/crates/l/adk-server.svg)](LICENSE)

## Overview

`adk-server` provides HTTP infrastructure for ADK agents:

- **REST API** - Standard HTTP endpoints for agent interaction
- **A2A Protocol** - Agent-to-Agent communication (JSON-RPC 2.0)
- **SSE Streaming** - Server-Sent Events for real-time responses
- **Web UI** - Built-in chat interface for testing
- **RemoteA2aAgent** - Connect to remote agents as sub-agents

## Installation

```toml
[dependencies]
adk-server = "0.1"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "0.1", features = ["server"] }
```

## Quick Start

### Basic Server

```rust
use adk_server::{create_app, ServerConfig};
use std::sync::Arc;

let config = ServerConfig {
    agent_loader: Arc::new(SingleAgentLoader::new(Arc::new(agent))),
    session_service: Arc::new(InMemorySessionService::new()),
    artifact_service: None,
    backend_url: None,
};

let app = create_app(config);

let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
axum::serve(listener, app).await?;
```

### A2A Server

```rust
use adk_server::create_app_with_a2a;

let app = create_app_with_a2a(config, Some("http://localhost:8080"));

// Exposes:
// GET  /.well-known/agent.json  - Agent card
// POST /a2a                      - JSON-RPC endpoint
// POST /a2a/stream               - SSE streaming
```

### Remote Agent Client

```rust
use adk_server::RemoteA2aAgent;

let remote = RemoteA2aAgent::builder("weather_agent")
    .description("Remote weather service")
    .agent_url("http://weather-service:8080")
    .build()?;

// Use as sub-agent
let coordinator = LlmAgentBuilder::new("coordinator")
    .sub_agent(Arc::new(remote))
    .build()?;
```

## API Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/` | GET | Web UI |
| `/api/chat` | POST | Send message |
| `/api/chat/stream` | POST | Stream response |
| `/.well-known/agent.json` | GET | A2A agent card |
| `/a2a` | POST | A2A JSON-RPC |
| `/a2a/stream` | POST | A2A streaming |

## Features

- Axum-based async HTTP
- CORS support
- Embedded web assets
- Multi-agent routing
- Health checks

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-runner](https://crates.io/crates/adk-runner) - Execution runtime
- [adk-cli](https://crates.io/crates/adk-cli) - CLI launcher

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://github.com/zavora-ai/adk-rust) framework for building AI agents in Rust.
