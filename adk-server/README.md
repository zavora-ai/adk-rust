# adk-server

HTTP server and A2A protocol for Rust Agent Development Kit (ADK-Rust) agents.

[![Crates.io](https://img.shields.io/crates/v/adk-server.svg)](https://crates.io/crates/adk-server)
[![Documentation](https://docs.rs/adk-server/badge.svg)](https://docs.rs/adk-server)
[![License](https://img.shields.io/crates/l/adk-server.svg)](LICENSE)

## Overview

`adk-server` provides HTTP infrastructure for the Rust Agent Development Kit ([ADK-Rust](https://github.com/zavora-ai/adk-rust)):

- **REST API** - Standard HTTP endpoints for agent interaction
- **A2A Protocol** - Agent-to-Agent communication (JSON-RPC 2.0)
- **SSE Streaming** - Server-Sent Events for real-time responses
- **Web UI** - Built-in chat interface for testing
- **RemoteA2aAgent** - Connect to remote agents as sub-agents

## Installation

```toml
[dependencies]
adk-server = "0.3.1"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "0.3.1", features = ["server"] }
```

## Quick Start

### Basic Server

```rust
use adk_server::{create_app, ServerConfig};
use std::sync::Arc;

let config = ServerConfig::new(
    Arc::new(SingleAgentLoader::new(Arc::new(agent))),
    Arc::new(InMemorySessionService::new()),
);

let app = create_app(config);

let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
axum::serve(listener, app).await?;
```

### Security Configuration

Configure CORS, timeouts, and other security settings:

```rust
use adk_server::{ServerConfig, SecurityConfig};
use std::time::Duration;

// Development mode (permissive CORS, detailed errors)
let config = ServerConfig::new(agent_loader, session_service)
    .with_security(SecurityConfig::development());

// Production mode (restricted CORS, sanitized errors)
let config = ServerConfig::new(agent_loader, session_service)
    .with_allowed_origins(vec!["https://myapp.com".to_string()])
    .with_request_timeout(Duration::from_secs(60))
    .with_max_body_size(5 * 1024 * 1024);  // 5MB
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

### Runtime and Sessions

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/health` | GET | Health check |
| `/api/apps` | GET | List available agents |
| `/api/list-apps` | GET | adk-go compatible app listing |
| `/api/sessions` | POST | Create session |
| `/api/sessions/{app_name}/{user_id}/{session_id}` | GET, DELETE | Get or delete session |
| `/api/run/{app_name}/{user_id}/{session_id}` | POST | Run agent with SSE |
| `/api/run_sse` | POST | adk-go compatible SSE runtime |

### UI Protocol Contracts

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/ui/capabilities` | GET | Supported UI protocols/features (`adk_ui`, `a2ui`, `ag_ui`, `mcp_apps`) |
| `/api/ui/resources` | GET | List MCP UI resources (`ui://` entries) |
| `/api/ui/resources/read?uri=...` | GET | Read a registered MCP UI resource |
| `/api/ui/resources/register` | POST | Register an MCP UI resource (validated `ui://` + mime/meta) |

Runtime endpoints support protocol negotiation via:
- request body field `uiProtocol` / `ui_protocol`
- header `x-adk-ui-protocol` (takes precedence)

Supported runtime profile values:
- `adk_ui` (default, legacy profile)
- `a2ui`
- `ag_ui`
- `mcp_apps`

Deprecation signaling:

- `adk_ui` deprecation metadata is included in `/api/ui/capabilities`.
- Runtime requests using `adk_ui` emit server warning logs to aid migration tracking.
- Current timeline: announced `2026-02-07`, sunset target `2026-12-31`.

Example runtime request:

```bash
curl -X POST "http://localhost:8080/api/run_sse" \
  -H "content-type: application/json" \
  -H "x-adk-ui-protocol: ag_ui" \
  -d '{
    "appName": "assistant",
    "userId": "user1",
    "sessionId": "session1",
    "newMessage": {
      "parts": [{ "text": "Render a dashboard." }],
      "role": "user"
    }
  }'
```

Protocol response behavior:

- `adk_ui` profile: legacy runtime event payload shape
- non-default profiles (`a2ui`, `ag_ui`, `mcp_apps`): profile-wrapped SSE payloads with protocol metadata

MCP UI resource registration request shape:

```json
{
  "uri": "ui://demo/dashboard",
  "mimeType": "text/html;profile=mcp-app",
  "text": "<html>...</html>",
  "meta": {
    "ui": {
      "domain": "https://example.com"
    }
  }
}
```

Resource registration enforces:

- `ui://` URI scheme
- supported MIME type contracts
- metadata domain/CSP validation

### A2A Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
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

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
