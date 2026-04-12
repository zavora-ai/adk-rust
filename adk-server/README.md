# adk-server

HTTP server and A2A v1.0.0 protocol for Rust Agent Development Kit (ADK-Rust) agents.

[![Crates.io](https://img.shields.io/crates/v/adk-server.svg)](https://crates.io/crates/adk-server)
[![Documentation](https://docs.rs/adk-server/badge.svg)](https://docs.rs/adk-server)
[![License](https://img.shields.io/crates/l/adk-server.svg)](LICENSE)

## Overview

`adk-server` provides HTTP infrastructure for the Rust Agent Development Kit ([ADK-Rust](https://github.com/zavora-ai/adk-rust)):

- **REST API** - Standard HTTP endpoints for agent interaction
- **A2A Protocol** - Agent-to-Agent v1.0.0 communication (JSON-RPC 2.0, all 11 operations)
- **SSE Streaming** - Server-Sent Events for real-time responses
- **Web UI** - Built-in chat interface for testing
- **RemoteA2aAgent** - Connect to remote agents as sub-agents
- **Auth Bridge** - Flow authenticated identity from HTTP headers into agent execution
- **Artifacts** - Binary artifact storage and retrieval per session
- **Debug/Tracing** - Trace inspection and graph visualization endpoints

## Installation

```toml
[dependencies]
adk-server = "0.5.0"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "0.5.0", features = ["server"] }
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
    .with_security(SecurityConfig::production(vec!["https://myapp.com".to_string()]));

// Or configure individual settings
let config = ServerConfig::new(agent_loader, session_service)
    .with_allowed_origins(vec!["https://myapp.com".to_string()])
    .with_request_timeout(Duration::from_secs(60))
    .with_max_body_size(5 * 1024 * 1024)  // 5MB
    .with_error_details(false);
```

### Optional Services

```rust
let config = ServerConfig::new(agent_loader, session_service)
    .with_artifact_service(Arc::new(artifact_service))
    .with_memory_service(Arc::new(memory_service))
    .with_span_exporter(Arc::new(span_exporter))
    .with_request_context(Arc::new(my_auth_extractor));
```

### Runner Configuration Passthrough

`ServerConfig` can now forward runner-level compaction and prompt-cache settings:

```rust
let config = ServerConfig::new(agent_loader, session_service)
    .with_compaction(compaction_config)
    .with_context_cache(context_cache_config, cache_capable_model);
```

This applies to both the standard SSE runtime endpoints and the A2A runtime
controller.

### A2A v1.0.0 Server

```rust
use adk_server::create_app_with_a2a;

let app = create_app_with_a2a(config, Some("http://localhost:8080"));

// Exposes:
// GET  /.well-known/agent-card.json  - Agent card with capabilities
// POST /jsonrpc                       - JSON-RPC endpoint (all 11 v1 operations)
// REST routes for all operations
// A2A-Version header negotiation
```

The A2A v1.0.0 implementation includes: RFC 3339 timestamps, capabilities declaration, message ID idempotency, push notification authentication, INPUT_REQUIRED multi-turn flow, input validation, `application/a2a+json` Content-Type, and Task-as-first-SSE-event. See [A2A docs](../docs/official_docs/deployment/a2a.md) for details.

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

### Auth Bridge

Flow authenticated identity from HTTP requests into agent execution:

```rust
use adk_server::auth_bridge::{RequestContextExtractor, RequestContextError};
use adk_core::RequestContext;
use async_trait::async_trait;

struct MyExtractor;

#[async_trait]
impl RequestContextExtractor for MyExtractor {
    async fn extract(
        &self,
        parts: &axum::http::request::Parts,
    ) -> Result<RequestContext, RequestContextError> {
        let auth = parts.headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or(RequestContextError::MissingAuth)?;
        // validate token, build RequestContext ...
        todo!()
    }
}

let config = ServerConfig::new(agent_loader, session_service)
    .with_request_context(Arc::new(MyExtractor));
```

When configured, the extracted `RequestContext` flows into `InvocationContext`, making scopes available to tools via `ToolContext::user_scopes()`. Session and artifact endpoints enforce user_id authorization against the authenticated identity.

## API Endpoints

### Health

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/health` | GET | Health check with component status |

### Apps

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/apps` | GET | List available agents |
| `/api/list-apps` | GET | adk-go compatible app listing |

### Sessions

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/sessions` | POST | Create session |
| `/api/sessions/{app_name}/{user_id}/{session_id}` | GET, DELETE | Get or delete session |
| `/api/apps/{app_name}/users/{user_id}/sessions` | GET, POST | List or create sessions |
| `/api/apps/{app_name}/users/{user_id}/sessions/{session_id}` | GET, POST, DELETE | Get, create, or delete session |

### Runtime

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/run/{app_name}/{user_id}/{session_id}` | POST | Run agent with SSE |
| `/api/run_sse` | POST | adk-go compatible SSE runtime |

### Artifacts

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/sessions/{app_name}/{user_id}/{session_id}/artifacts` | GET | List artifacts for a session |
| `/api/sessions/{app_name}/{user_id}/{session_id}/artifacts/{artifact_name}` | GET | Get a specific artifact |

### Debug and Tracing

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/debug/trace/{event_id}` | GET | Get trace by event ID (admin only when auth configured) |
| `/api/debug/trace/session/{session_id}` | GET | Get all spans for a session |
| `/api/debug/graph/{app_name}/{user_id}/{session_id}/{event_id}` | GET | Get graph visualization |
| `/api/apps/{app_name}/users/{user_id}/sessions/{session_id}/events/{event_id}` | GET | Get event data |
| `/api/apps/{app_name}/users/{user_id}/sessions/{session_id}/events/{event_id}/graph` | GET | Get graph (path-style) |
| `/api/apps/{app_name}/eval_sets` | GET | Get evaluation sets (stub) |

### UI Protocol

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/ui/capabilities` | GET | Supported UI protocols plus capability metadata (`versions`, `features`, `implementationTier`, `specTrack`, `summary`, `limitations`) |
| `/api/ui/initialize` | POST | Additive MCP Apps host-bridge initialize helper (direct body or JSON-RPC-like envelope) |
| `/api/ui/message` | POST | Additive MCP Apps host-bridge message helper |
| `/api/ui/update-model-context` | POST | Additive MCP Apps host-bridge model-context helper |
| `/api/ui/notifications/poll` | POST | Poll queued MCP Apps host-bridge notifications |
| `/api/ui/notifications/resources-list-changed` | POST | Queue an MCP Apps resource-list-changed notification |
| `/api/ui/notifications/tools-list-changed` | POST | Queue an MCP Apps tool-list-changed notification |
| `/api/ui/resources` | GET | List MCP UI resources (`ui://` entries) |
| `/api/ui/resources/read?uri=...` | GET | Read a registered MCP UI resource |
| `/api/ui/resources/register` | POST | Register an MCP UI resource (validated `ui://` + mime/meta) |

Runtime endpoints support protocol negotiation via:
- request body field `uiProtocol` / `ui_protocol`
- header `x-adk-ui-protocol` (takes precedence)
- request body field `uiTransport` / `ui_transport`
- header `x-adk-ui-transport` (takes precedence)

Supported runtime profile values: `adk_ui` (default), `a2ui`, `ag_ui`, `mcp_apps`.

Current support is intentionally tiered:
- `a2ui` is a draft-aligned hybrid subset exposed through protocol-aware UI tool payloads.
- `ag_ui` is a hybrid subset: the default stream remains the generic ADK wrapper, but clients can opt into `protocol_native` transport plus AG-UI run input fields on `/api/run_sse`.
- `mcp_apps` is a compatibility subset with `ui://` resource registration plus additive `initialize` / `message` / `update-model-context` bridge helpers, notification polling, list-changed host flows, and runtime request fields, not a full browser `postMessage` host bridge yet.

Runtime transport values:
- `legacy_wrapper` (default) preserves the existing generic ADK SSE envelope.
- `protocol_native` is currently available for `ag_ui` only.

Use `/api/ui/capabilities` instead of assuming full upstream protocol parity.

For MCP Apps tool responses, `adk-server::ui_types` now exposes a canonical additive helper:
- `McpUiBridgeSnapshot` for typed host/app bridge state that can be promoted into tool responses
- `McpUiToolResult` for the shared tool-result envelope
- `McpUiToolResultBridge` for typed bridge metadata (`protocolVersion`, `structuredContent`, `hostInfo`, `hostCapabilities`, `hostContext`, `appInfo`, `appCapabilities`, `initialized`)

Use `McpUiBridgeSnapshot::build_tool_result(...)` as the preferred constructor path when promoting framework bridge state into an MCP Apps tool response. `resourceUri` and inline `html` fallbacks remain available for compatibility-oriented hosts.

For embedded-host mappings, the additive HTTP bridge corresponds to MCP Apps host/app methods as follows:
- `ui/initialize` -> `/api/ui/initialize`
- `ui/message` -> `/api/ui/message`
- `ui/update-model-context` -> `/api/ui/update-model-context`
- `notifications/resources/list_changed` -> `/api/ui/notifications/resources-list-changed`
- `notifications/tools/list_changed` -> `/api/ui/notifications/tools-list-changed`
- queued host notifications -> `/api/ui/notifications/poll`

### A2A Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/.well-known/agent.json` | GET | A2A agent card |
| `/a2a` | POST | A2A JSON-RPC |
| `/a2a/stream` | POST | A2A streaming |

### Web UI

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/` | GET | Redirect to `/ui/` |
| `/ui/` | GET | Built-in chat interface |
| `/ui/assets/config/runtime-config.json` | GET | Runtime configuration |
| `/ui/{*path}` | GET | Static UI assets |

## Security

The server applies the following security layers automatically:

- CORS (configurable allowed origins)
- Request body size limits (default 10MB)
- Request timeouts (default 30s)
- `X-Content-Type-Options: nosniff`
- `X-Frame-Options: DENY`
- `X-XSS-Protection: 1; mode=block`
- Request ID tracking via `x-request-id` header
- User ID authorization on session/artifact/debug endpoints when auth is configured

## Features

- Axum-based async HTTP server
- CORS support with configurable origins
- Embedded web UI assets
- Multi-agent routing via `AgentLoader`
- Health checks with component status
- OpenTelemetry trace integration
- Auth middleware bridge for identity propagation
- Artifact storage and retrieval
- A2A v1.0.0 protocol with JSON-RPC 2.0 (all 11 operations, idempotency, multi-turn, push auth)

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-runner](https://crates.io/crates/adk-runner) - Execution runtime
- [adk-cli](https://crates.io/crates/adk-cli) - CLI launcher
- [adk-telemetry](https://crates.io/crates/adk-telemetry) - OpenTelemetry integration
- [adk-artifact](https://crates.io/crates/adk-artifact) - Artifact storage
- [adk-auth](https://crates.io/crates/adk-auth) - Authentication (JWT bridge)
- [adk-ui](https://crates.io/crates/adk-ui) - UI protocol support

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
