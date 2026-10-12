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
- **Runtime UI** - Built-in responsive interface for agent and team execution
- **RemoteA2aAgent** - Connect to remote agents as sub-agents
- **Auth Bridge** - Flow authenticated identity from HTTP headers into agent execution
- **Artifacts** - Binary artifact storage and retrieval per session
- **Debug/Tracing** - Trace inspection and graph visualization endpoints
- **YAML Agent Config** (feature: `yaml-agent`) - Declarative agent definitions with:
  - `AgentConfigLoader` — load agents from YAML files
  - `HotReloadWatcher` — filesystem watching with debounce
  - Environment variable interpolation (`${VAR}` and `${VAR:-default}`)
  - Plugin, session, and memory backend configuration in YAML
  - Round-trip serialization (`serialize_definition()`)

## Installation

```toml
[dependencies]
adk-server = "3.0.0"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "3.0.0", features = ["server"] }
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
```

### ServerBuilder — Custom Routes

`ServerBuilder` lets you register custom Axum controllers alongside the built-in
routes. Custom routes share the same middleware stack (auth, CORS, tracing,
timeout, security headers).

```rust
use adk_server::{ServerBuilder, ServerConfig};
use axum::{Router, routing::get, Json};

let app = ServerBuilder::new(config)
    // Routes nested under /api — get auth middleware automatically
    .add_api_routes(
        Router::new()
            .route("/projects", get(list_projects))
            .route("/projects/{id}", get(get_project))
    )
    // Root-level routes — CORS/tracing/security headers, but no auth middleware
    .add_root_routes(
        Router::new()
            .route("/webhook", axum::routing::post(handle_webhook))
    )
    // Enable A2A protocol
    .with_a2a("http://localhost:8080")
    .build();

let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
axum::serve(listener, app).await?;
```

| Method | Description |
|--------|-------------|
| `ServerBuilder::new(config)` | Create a builder from a `ServerConfig` |
| `.add_api_routes(router)` | Add routes nested under `/api` with auth middleware |
| `.add_root_routes(router)` | Add routes at the root level (no auth middleware) |
| `.with_a2a(base_url)` | Enable A2A protocol endpoints |
| `.enable_shutdown_endpoint()` | Enable `POST /api/shutdown` for graceful shutdown |
| `.build()` | Build the final `axum::Router` with all middleware |
| `.build_with_shutdown()` | Build and return a `ShutdownHandle` for graceful shutdown |

See [`examples/server_builder/`](../examples/server_builder/) for a complete working example.

### Graceful Shutdown Endpoint

Enable `POST /api/shutdown` for clean process termination (e.g., from an Electron shell):

```rust
use adk_server::{ServerBuilder, ServerConfig};

let (app, shutdown_handle) = ServerBuilder::new(config)
    .enable_shutdown_endpoint()
    .build_with_shutdown();

let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
axum::serve(listener, app)
    .with_graceful_shutdown(shutdown_handle.signal())
    .await?;
```

When `POST /api/shutdown` is called, the server:
1. Stops accepting new connections
2. Completes in-flight requests
3. Flushes pending writes (SQLite WAL, etc.)
4. Exits with code 0

The `ShutdownHandle::signal()` also responds to Ctrl+C and SIGTERM, combining all shutdown triggers into one future.

// Exposes:
// GET  /.well-known/agent-card.json  - Agent card with capabilities
// POST /jsonrpc                       - JSON-RPC endpoint (all 11 v1 operations)
// REST routes for all operations
// A2A-Version header negotiation
```

The A2A v1.0.0 implementation includes: RFC 3339 timestamps, capabilities declaration, message ID idempotency, push notification authentication, INPUT_REQUIRED multi-turn flow, input validation, `application/a2a+json` Content-Type, and Task-as-first-SSE-event. See [A2A docs](../docs/official_docs/deployment/a2a.md) for details.

### Governance Kill Switch

`ServerConfig::governance` is shared by every runner the server builds. Freezing it fails new
runs and stops running ones before their next model or tool call.
`ServerBuilder::enable_governance_endpoints()` mounts `POST /api/admin/freeze`,
`POST /api/admin/unfreeze`, and `GET /api/admin/governance` behind the auth middleware; a
freeze also pauses background-run and cron scheduling mounted on the same builder.

```rust,ignore
let control = adk_core::GovernanceControl::new();
let config = ServerConfig::new(loader, sessions).with_governance(control.clone());
let app = ServerBuilder::new(config).enable_governance_endpoints().build();
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

When configured, the extracted `RequestContext` flows into `InvocationContext`, making scopes available to tools via `ToolContext::user_scopes()`. Session and artifact endpoints enforce user_id authorization against the authenticated identity. A2A routes use the authenticated identity as the session user and the owner of every task they create, so `tasks/get` and `tasks/cancel` cannot reach another caller's task.

### Background Runs and Cron (feature: `background`)

Runs a workflow or an agent without holding the request open, and schedules one on a cron
expression. Neither is compiled unless the feature is on.

`BackgroundState` holds a `DurableRunStore` — an `InMemoryRunStore` by default — and the
targets runs execute. A `DurableWorker` over the same store claims runs under fenced leases;
unless disabled, one starts in this process when the router is built inside a Tokio runtime.

```rust,ignore
use std::sync::Arc;

use adk_server::background::{BackgroundState, CronState, DurableWorkerConfig, WorkflowRegistry};

let background = BackgroundState::new()
    .with_run_store(run_store)                 // shared across replicas
    .with_executor(Arc::new(WorkflowRegistry::new()))
    .with_agent("support", Arc::new(runner))   // runs whose target is "support"
    .with_worker_config(DurableWorkerConfig::default().with_queue("default"));
let cron = CronState::new(background.clone()).with_schedule_store(schedule_store);
adk_server::background::start_cron_scheduler(cron.clone()); // on every replica
let app = ServerBuilder::new(config)
    .with_background_runs(background)
    .with_cron_jobs(cron)
    .build();
```

| `BackgroundState` method | Effect |
|--------------------------|--------|
| `with_run_store` | Uses a shared `DurableRunStore`; every replica claiming from it shares the work |
| `with_executor` | Workflows a run may target |
| `with_agent` | Runs whose target names the agent execute through its `Runner` |
| `with_embedded_worker(false)` | No worker in this process (API-only replicas) |
| `with_worker_config` | Queue, lease, poll interval, concurrency, run timeout, retention |
| `with_governance` | Kill switch; `ServerBuilder` installs `ServerConfig::governance` when none is set |

| Endpoint | Method | Purpose |
|----------|--------|---------|
| `/runs` | POST | Enqueue a run, returning its id immediately |
| `/runs` | GET | List runs (`state`, `sessionId`, `limit`) |
| `/runs/{id}` | GET | Read a run's status, attempts, checkpoint, and result |
| `/runs/{id}` | DELETE | Cancel a run; a running run stops at its worker's next heartbeat, on whichever replica |
| `/runs/{id}/wake` | POST | Make a run parked on a held tool call claimable now; the approval service calls it after a decision |
| `/cron` | POST / GET | Create and list cron jobs |
| `/cron/{id}` | GET / PATCH / DELETE | Read, pause or resume, and delete a cron job |

`POST /runs` takes `target` (or `workflowId`), `input`, `sessionId`, `idempotencyKey`,
`concurrencyKey`, `onConflict` (`reject`, `enqueue`, `interrupt`), `notBefore`, and
`maxRetries`. An existing idempotency key returns the existing run with `200` and
`"existing": true`; a `reject` conflict returns `409` with `blockingRunId`; an unknown target
returns `404`; a frozen or stale kill switch returns `503`.

A cron job is a schedule whose id is its name, evaluated in `timezone` (IANA, default `UTC`),
with a `missedFirePolicy` for occurrences missed while the scheduler was behind.

| `concurrencyPolicy` | Run template |
|---------------------|--------------|
| `skip` (default) | Concurrency key `cron:{name}`, `OnConflict::Reject` |
| `queue` | Concurrency key `cron:{name}`, `OnConflict::Enqueue` |
| `allow` | No concurrency key |

Each fire time is enqueued once however many replicas run the scheduler, and resuming a
paused job skips the occurrences that fell inside the pause.

> **Important:** the routers carry no authentication. Mount them with
> `ServerBuilder::with_background_runs` and `ServerBuilder::with_cron_jobs`, which
> serve `/api/runs` and `/api/cron` behind the server's auth middleware, or put a
> standalone mount behind your own authentication layer.

**What survives a restart.** With a durable run store, everything: a run whose worker stopped
is recovered by another worker once its lease expires and resumes from its checkpoint (see
[Durable Runtime](../docs/official_docs/core/durable-runtime.md#crash-resume)). Finished runs
are pruned after `DurableWorkerConfig::retain_finished` (24 hours by default).

**Whose identity a run uses.** The worker acts as the runner's own principal, for the run's
user. A run's stored principal is honoured only for agent ids listed with
`DurableWorkerConfig::with_allowed_principal` (none by default); any other fails the run with
`identity.principal_not_allowed`, recorded on the audit trail. Runs from the scheduler, webhook
hooks, and queue ingress act for their user as a service (see
[Agent Identity](../docs/official_docs/security/agent-identity.md#durable-runs)).

### Webhook Wake-ups (feature: `webhook-ingress`)

`ServerBuilder::with_webhook_ingress` serves `POST /hooks/{name}`. Each `WebhookHook` verifies
Standard Webhooks signatures and maps the delivery to a `NewRun` in a `DurableRunStore`, keyed
by the `webhook-id`:

| Status | Meaning |
|--------|---------|
| `202` | New run enqueued: `{"runId", "state", "duplicate": false}` |
| `200` | Redelivery of a `webhook-id` that already has a run; the same run is returned |
| `401` | Signature missing, invalid, or outside the timestamp tolerance |
| `409` | `OnConflict::Reject` and an unfinished run holds the hook's concurrency key |
| `422` | Body is not JSON, or a key template names a missing field |
| `429` | The hook's `max_pending` cap is reached (`Retry-After`) |
| `503` | The kill switch is frozen or stale, or the run store is unreachable (`Retry-After`) |

Share the run store with `BackgroundState::with_run_store` and the embedded `DurableWorker` runs
woken runs like any other. With a mirrored `GovernanceControl` as `ServerConfig::governance`, a
freeze written by any replica refuses deliveries on every replica.

See [Wake-ups and Webhooks](../docs/official_docs/core/wake-ups.md).

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
| `/api/ui/agents/{name}` | GET | Agent capabilities, hierarchy, and exact portable team topology |
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
| `/.well-known/agent-card.json` | GET | A2A agent card |
| `/.well-known/agent.json` | GET | A2A agent card (pre-0.3 path) |
| `/a2a` | POST | A2A JSON-RPC |
| `/a2a/stream` | POST | A2A streaming |

### Web UI

The ADK-Rust-owned interface uses the Studio Next visual language while staying
focused on execution. It displays Markdown-formatted conversations and multimodal attachments,
streaming tool activity, handoffs, animated exact team delegation/handoff topology, realtime
transcripts and completed audio, event timelines, dedicated telemetry spans, session and shared
state, artifacts, prior sessions, configured runtime services, A2A discovery, and negotiated UI
and MCP Apps protocols. It is responsive, keyboard accessible, supports system/light/dark themes,
and has no external font or asset dependency. The **ADK Runtime** brand links to
[adk-rust.com](https://adk-rust.com).

See [`examples/advanced_agents/`](../examples/advanced_agents/) for one OpenAI-backed server
demonstrating ambient scheduling, realtime voice, A2A, MCP discovery/tasks, and telemetry.

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/` | GET | Redirect to `/ui/` |
| `/ui/` | GET | Built-in agent and team runtime interface |
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
