# adk-server

HTTP server and A2A protocol for Rust Agent Development Kit (ADK-Rust) agents.

[![Crates.io](https://img.shields.io/crates/v/adk-server.svg)](https://crates.io/crates/adk-server)
[![Documentation](https://docs.rs/adk-server/badge.svg)](https://docs.rs/adk-server)
[![License](https://img.shields.io/crates/l/adk-server.svg)](../LICENSE)

## Overview

`adk-server` provides HTTP infrastructure for the Rust Agent Development Kit ([ADK-Rust](https://github.com/zavora-ai/adk-rust)):

- **REST API** — session, runtime (SSE and JSON), artifact, and debug endpoints
- **A2A** — agent card discovery and JSON-RPC endpoints, a client (`A2aClient`), and `RemoteA2aAgent` for remote agents as sub-agents; the `a2a-v1` feature adds the A2A v1.0.0 handlers for all 11 operations
- **Custom routes** — `ServerBuilder` mounts routers such as `adk-surface`'s web home behind the same middleware
- **Security defaults** — loopback hosts only, no cross-origin access, body and time limits, security headers
- **Auth bridge** — flows authenticated identity from HTTP requests into agent execution
- **Governance** — a kill switch shared by every runner the server builds, with admin endpoints
- **Durable background runs, cron, webhook wake-ups, and approvals** — behind features

## Installation

```toml
[dependencies]
adk-server = "3.0.0"
```

Or use the umbrella crate (the `server` feature, in the `standard` tier and above, also enables `a2a-v1`):

```toml
[dependencies]
adk-rust = { version = "3.0.0", features = ["standard"] }
```

## Feature Flags

No feature is enabled by default.

| Feature | Enables |
|---------|---------|
| `a2a-v1` | The `a2a::v1` module (A2A v1.0.0 JSON-RPC and REST handlers, v1 client) and `A2aServer` |
| `a2a-interceptors` | Built-in A2A interceptors (auth, rate limit, audit) |
| `background` | Durable background runs (`/api/runs`), cron jobs (`/api/cron`), `DurableWorker`, and `DurableScheduler` |
| `webhook-ingress` | `ServerBuilder::with_webhook_ingress`: signed `POST /hooks/{name}` deliveries that enqueue durable runs |
| `approvals` | `ServerBuilder::with_approval_service`: the `/api/approvals` inbox, `spawn_approval_ticker`, and `WebhookApprovalNotifier` |
| `openai-webhooks` | `OpenAIWebhookHandler` for OpenAI background-task completion events |
| `yaml-agent` | `AgentConfigLoader` and `HotReloadWatcher` for YAML agent definitions |
| `agent-registry` | The agent registry REST API |
| `agent-engine` | The Gemini Enterprise Agent Platform (ReasoningEngine) dispatch surface and `serve_agent_engine` |
| `vertex-remote-engine` | `RemoteReasoningEngineAgent`: deployed Agent Engine agents as sub-agents (implies `agent-engine`) |
| `awp` | `ServerBuilder::awp(AwpEdge)`: the AWP 2.0 agent edge — trust tiers from verified credentials, capabilities over A2A, MCP and HTTP through one governed path, held and exactly-once calls, the public partition's conversation endpoints, event subscriptions, consent receipts and the beacon (implies `a2a-v1`) |
| `awp-sqlite`, `awp-postgres`, `awp-redis` | The edge stores and shared rate-limit windows in SQLite, PostgreSQL or Redis (implies `awp`) |

## Quick Start

```rust
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_core::SingleAgentLoader;
use adk_model::GeminiModel;
use adk_server::{ServerConfig, create_app};
use adk_session::InMemorySessionService;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-3.7-flash")?);
    let agent = LlmAgentBuilder::new("assistant").model(model).build()?;

    let config = ServerConfig::new(
        Arc::new(SingleAgentLoader::new(Arc::new(agent))),
        Arc::new(InMemorySessionService::new()),
    );
    let app = create_app(config);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;
    axum::serve(listener, app).await?;
    Ok(())
}
```

The app name is the agent's name. Create a session, then run the agent over SSE:

```bash
curl -X POST http://127.0.0.1:8080/api/apps/assistant/users/user/sessions/s1
curl -N -X POST http://127.0.0.1:8080/api/run/assistant/user/s1 \
  -H 'content-type: application/json' -d '{"new_message": "Hello"}'
```

### Security Configuration

The server answers only `localhost`, `*.localhost`, and IP addresses in the `Host` header (plus the hosts of the A2A base URL and the backend URL), and allows no cross-origin browser access: a request for any other host, or from any other origin, is refused with `403`. List what else reaches the server:

```rust
use std::sync::Arc;
use std::time::Duration;

use adk_core::AgentLoader;
use adk_server::{SecurityConfig, ServerConfig};
use adk_session::SessionService;

fn configs(loader: Arc<dyn AgentLoader>, sessions: Arc<dyn SessionService>) -> Vec<ServerConfig> {
    vec![
        // Public host names and the browser origins that call the API
        ServerConfig::new(loader.clone(), sessions.clone())
            .with_allowed_hosts(vec!["agents.example.com".to_string()])
            .with_allowed_origins(vec!["https://app.example.com".to_string()]),
        // Development: loopback origins on any port, detailed A2A errors, admin debug endpoints
        ServerConfig::new(loader.clone(), sessions.clone())
            .with_security(SecurityConfig::development()),
        // Production: CORS for the listed origins, sanitized A2A errors
        ServerConfig::new(loader.clone(), sessions.clone())
            .with_security(SecurityConfig::production(vec!["https://myapp.com".to_string()])),
        // Individual settings
        ServerConfig::new(loader, sessions)
            .with_allowed_origins(vec!["https://myapp.com".to_string()])
            .with_request_timeout(Duration::from_secs(60))
            .with_max_body_size(5 * 1024 * 1024) // 5 MB
            .with_error_details(false),
    ]
}
```

| `allowed_origins` entry | Matches |
|-------------------------|---------|
| `https://app.example.com` | That origin |
| `http://localhost:*` | That scheme and host on any port |
| `*` | Every origin |

| `allowed_hosts` entry | Matches |
|-----------------------|---------|
| `agents.example.com` | That host |
| `.example.com` | The domain and its subdomains |
| `*` | Every host |

Agent Engine routes skip the host check. A server that listens beyond loopback answers only the host names listed with `with_allowed_hosts`.

### Optional Services

```rust
use std::sync::Arc;

use adk_artifact::ArtifactService;
use adk_core::{CacheCapable, ContextCacheConfig, EventsCompactionConfig, Memory};
use adk_server::{RequestContextExtractor, ServerConfig};
use adk_telemetry::AdkSpanExporter;

fn with_services(
    config: ServerConfig,
    artifacts: Arc<dyn ArtifactService>,
    memory: Arc<dyn Memory>,
    spans: Arc<AdkSpanExporter>,
    auth: Arc<dyn RequestContextExtractor>,
    compaction: EventsCompactionConfig,
    cache: ContextCacheConfig,
    cache_model: Arc<dyn CacheCapable>,
) -> ServerConfig {
    config
        .with_artifact_service(artifacts)
        .with_memory_service(memory)
        .with_span_exporter(spans)
        .with_request_context(auth)
        .with_compaction(compaction)
        .with_context_cache(cache, cache_model)
}
```

Compaction and prompt-cache settings apply to the SSE runtime endpoints and the A2A controller.

### ServerBuilder — Custom Routes

`ServerBuilder` registers custom Axum routes beside the built-in ones. Custom routes share the middleware stack (CORS, host and origin checks, tracing, timeout, security headers); routes added with `add_api_routes` also get the auth middleware.

```rust
use axum::{Json, Router, routing::get, routing::post};
use adk_server::{ServerBuilder, ServerConfig};
use serde_json::{Value, json};

async fn list_projects() -> Json<Value> {
    Json(json!([]))
}

async fn handle_webhook() -> &'static str {
    "ok"
}

fn app(config: ServerConfig) -> Router {
    ServerBuilder::new(config)
        // Nested under /api, behind the auth middleware
        .add_api_routes(Router::new().route("/projects", get(list_projects)))
        // Root level, without the auth middleware
        .add_root_routes(Router::new().route("/webhook", post(handle_webhook)))
        // A2A discovery and JSON-RPC
        .with_a2a("http://localhost:8080")
        .build()
}
```

| Method | Description |
|--------|-------------|
| `ServerBuilder::new(config)` | Create a builder from a `ServerConfig` |
| `.add_api_routes(router)` | Add routes nested under `/api` with auth middleware |
| `.add_root_routes(router)` | Add routes at the root level (no auth middleware) |
| `.with_a2a(base_url)` | Mount the A2A discovery and JSON-RPC endpoints |
| `.with_skill_index(index)` / `.with_a2a_task_retention(retention)` | A2A agent-card skills and task retention |
| `.enable_shutdown_endpoint()` | Mount `POST /api/shutdown` |
| `.enable_governance_endpoints()` | Mount the kill-switch admin endpoints |
| `.with_background_runs(state)` / `.with_cron_jobs(state)` | Mount `/api/runs` and `/api/cron` (`background`) |
| `.with_webhook_ingress(ingress)` | Mount `POST /hooks/{name}` (`webhook-ingress`) |
| `.with_approval_service(service)` | Mount `/api/approvals` (`approvals`) |
| `.with_agent_engine(true)` | Mount the Agent Engine dispatch routes (`agent-engine`) |
| `.build()` | Build the `axum::Router` |
| `.build_with_shutdown()` | Build and return a `ShutdownHandle` |

See [`examples/server_builder/`](../examples/server_builder/) for a complete working example.

### Graceful Shutdown

```rust
use adk_server::{ServerBuilder, ServerConfig};

async fn serve(config: ServerConfig) -> std::io::Result<()> {
    let (app, shutdown_handle) =
        ServerBuilder::new(config).enable_shutdown_endpoint().build_with_shutdown();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;
    axum::serve(listener, app).with_graceful_shutdown(shutdown_handle.signal()).await
}
```

`ShutdownHandle::signal()` resolves on `POST /api/shutdown`, `ShutdownHandle::shutdown()`, Ctrl+C, or SIGTERM; `axum::serve` then stops accepting connections and completes in-flight requests.

### Governance Kill Switch

`ServerConfig::governance` is shared by every runner the server builds. Freezing it fails new runs and stops running ones before their next model or tool call. `ServerBuilder::enable_governance_endpoints()` mounts `POST /api/admin/freeze`, `POST /api/admin/unfreeze`, and `GET /api/admin/governance` behind the auth middleware; a freeze stops scheduling and requests cancellation of running background runs — a terminate, not a pause — unless a `FreezeDelegate` (`ServerBuilder::with_freeze_delegate`), as a kernel registers, parks them instead; the responses report `parkedRuns`, `cancelledRuns` and `runningRuns` (`park` or `cancel`). A `DurableWorker` whose own control refuses a run after it made a call parks it at its next model-call boundary and wakes it on unfreeze (`DurableWorkerConfig::on_governance_refusal`). With a mirrored control (`GovernanceControl::mirrored`), freeze and unfreeze write the shared `GovernanceStateStore`, and the status reports `mirrored` and `stale`.

```rust
use adk_core::GovernanceControl;
use adk_server::{ServerBuilder, ServerConfig};

fn governed(config: ServerConfig) -> axum::Router {
    let control = GovernanceControl::new();
    let config = config.with_governance(control.clone());
    ServerBuilder::new(config).enable_governance_endpoints().build()
}
```

## A2A

`create_app_with_a2a(config, Some(base_url))`, or `ServerBuilder::with_a2a(base_url)`, mounts:

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/.well-known/agent-card.json` | GET | Agent card (public) |
| `/.well-known/agent.json` | GET | Agent card at the earlier path (public) |
| `/a2a` | POST | JSON-RPC: `message/send`, `tasks/get`, `tasks/cancel` (authenticated) |
| `/a2a/stream` | POST | JSON-RPC streaming: `message/stream`, `message/send` (authenticated) |

With an auth bridge, A2A routes use the authenticated identity as the session user and the owner of every task they create, so `tasks/get` and `tasks/cancel` cannot reach another caller's task. Model thinking is never sent to a peer.

The `a2a-v1` feature adds `adk_server::a2a::v1`: a `RequestHandler` with the A2A v1.0.0 JSON-RPC handler (`jsonrpc_handler`, all 11 operations from `SendMessage` to `GetExtendedAgentCard`) and REST router (`rest_router`), which an application mounts on its own routes, and `a2a::client::v1_client::A2aV1Client`. The v1 implementation covers RFC 3339 timestamps, capabilities declaration, message ID idempotency, push notification authentication, the `INPUT_REQUIRED` multi-turn flow, input validation, `application/a2a+json` responses, and the task as the first SSE event. See [`examples/a2a-research-agent`](../examples/a2a-research-agent) and [A2A docs](../docs/official_docs/deployment/a2a.md).

### Remote Agent Client

```rust
use std::sync::Arc;

use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::{Llm, Result};
use adk_server::RemoteA2aAgent;

fn coordinator(model: Arc<dyn Llm>) -> Result<LlmAgent> {
    let remote = RemoteA2aAgent::builder("weather_agent")
        .description("Remote weather service")
        .agent_url("http://weather-service:8080")
        .build()?;

    // Use as a sub-agent
    LlmAgentBuilder::new("coordinator").model(model).sub_agent(Arc::new(remote)).build()
}
```

The agent card's endpoint must be on `agent_url`'s origin, or on an origin added with `allow_endpoint_origin` (over `https` unless loopback). `A2aClient::resolve_agent_card` refuses a card naming any other host with `a2a.card_endpoint_refused`, and `RemoteA2aAgent` ends the run with an error event. `credential_broker` attaches a bearer token only to requests inside the token's audience. A stream that ends before its task reaches a final status is reported as an error event.

### Auth Bridge

Flow authenticated identity from HTTP requests into agent execution:

```rust
use std::sync::Arc;

use adk_core::RequestContext;
use adk_server::{RequestContextError, RequestContextExtractor, ServerConfig};
use async_trait::async_trait;

struct MyExtractor;

#[async_trait]
impl RequestContextExtractor for MyExtractor {
    async fn extract(
        &self,
        parts: &axum::http::request::Parts,
    ) -> Result<RequestContext, RequestContextError> {
        let token = parts
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .ok_or(RequestContextError::MissingAuth)?;
        // Validate the token before trusting it.
        let user_id = token.trim_start_matches("Bearer ").to_string();
        Ok(RequestContext { user_id, scopes: vec![], metadata: Default::default() })
    }
}

fn authenticated(config: ServerConfig) -> ServerConfig {
    config.with_request_context(Arc::new(MyExtractor))
}
```

The extracted `RequestContext` flows into `InvocationContext`, making scopes available to tools via `ToolContext::user_scopes()`. Session, artifact, and debug endpoints enforce the authenticated user. `adk-auth`'s `auth-bridge` feature provides a JWT extractor.

## Background Runs and Cron (feature: `background`)

Runs a workflow or an agent without holding the request open, and schedules one on a cron expression.

`BackgroundState` holds a `DurableRunStore` — an `InMemoryRunStore` by default — and the targets runs execute. A `DurableWorker` over the same store claims runs under fenced leases; unless disabled, one starts in this process when the router is built inside a Tokio runtime.

```rust
use std::sync::Arc;

use adk_core::{DurableRunStore, ScheduleStore};
use adk_runner::Runner;
use adk_server::background::{
    BackgroundState, CronState, DurableWorkerConfig, WorkflowRegistry, start_cron_scheduler,
};
use adk_server::{ServerBuilder, ServerConfig};

fn durable(
    config: ServerConfig,
    runner: Arc<Runner>,
    run_store: Arc<dyn DurableRunStore>,
    schedule_store: Arc<dyn ScheduleStore>,
) -> axum::Router {
    let background = BackgroundState::new()
        .with_run_store(run_store) // shared across replicas
        .with_executor(Arc::new(WorkflowRegistry::new()))
        .with_agent("support", runner) // runs whose target is "support"
        .with_worker_config(DurableWorkerConfig::default().with_queue("default"));
    let cron = CronState::new(background.clone()).with_schedule_store(schedule_store);
    start_cron_scheduler(cron.clone()); // on every replica
    ServerBuilder::new(config).with_background_runs(background).with_cron_jobs(cron).build()
}
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
| `/api/runs` | POST | Enqueue a run, returning its id immediately |
| `/api/runs` | GET | List runs (`state`, `sessionId`, `limit`) |
| `/api/runs/{id}` | GET | Read a run's status, attempts, checkpoint, and result |
| `/api/runs/{id}` | DELETE | Cancel a run; a running run stops at its worker's next heartbeat, on whichever replica |
| `/api/runs/{id}/wake` | POST | Make a run parked on a held tool call claimable now; the approval service calls it after a decision |
| `/api/cron` | POST / GET | Create and list cron jobs |
| `/api/cron/{id}` | GET / PATCH / DELETE | Read, pause or resume, and delete a cron job |

`POST /runs` takes `target` (or `workflowId`), `input`, `sessionId`, `idempotencyKey`, `concurrencyKey`, `onConflict` (`reject`, `enqueue`, `interrupt`), `notBefore`, and `maxRetries`. An existing idempotency key returns the existing run with `200` and `"existing": true`; a `reject` conflict returns `409` with `blockingRunId`; an unknown target returns `404`; a frozen or stale kill switch returns `503`.

**Who owns a run.** With a `RequestContextExtractor` (`ServerConfig::with_request_context`), a run belongs to the authenticated caller:

| Request | Another owner's run |
|---------|---------------------|
| `GET /runs` | Not listed; the list holds only the caller's runs |
| `GET`, `DELETE /runs/{id}`, `POST /runs/{id}/wake` | `404 run not found`, the answer for an unknown id |
| `POST /runs` with an `idempotencyKey` | Never returned; the key is scoped to the caller, so another user's identical key creates its own run |
| `POST /runs` with a `concurrencyKey` | Never rejects, queues behind, or interrupts it; the key is scoped to the caller |

Runs the scheduler and webhook hooks enqueue belong to their service user (`cron`, or the hook's user). Without an extractor there is no caller to check: every client reads, lists, cancels, and wakes every run, keys are shared, and the server logs a warning once.

A cron job is a schedule whose id is its name, evaluated in `timezone` (IANA, default `UTC`), with a `missedFirePolicy` for occurrences missed while the scheduler was behind. Creating a job with an existing name returns `409`.

| `concurrencyPolicy` | Run template |
|---------------------|--------------|
| `skip` (default) | Concurrency key `cron:{name}`, `OnConflict::Reject` |
| `queue` | Concurrency key `cron:{name}`, `OnConflict::Enqueue` |
| `allow` | No concurrency key |

Each fire time is enqueued once however many replicas run the scheduler, and resuming a paused job skips the occurrences that fell inside the pause.

> **Important:** the standalone routers (`background_runs_router`, `cron_jobs_router`) carry no authentication, so they apply no owner checks. `ServerBuilder::with_background_runs` and `ServerBuilder::with_cron_jobs` serve `/api/runs` and `/api/cron` behind the server's auth middleware; put a standalone mount behind your own authentication layer.

**What survives a restart.** With a durable run store, everything: a run whose worker stopped is recovered by another worker once its lease expires and resumes from its checkpoint (see [Durable Runtime](../docs/official_docs/core/durable-runtime.md#crash-resume)). Finished runs are pruned after `DurableWorkerConfig::retain_finished` (24 hours by default). An agent run that ends holding a tool call for approval is parked (until woken, or until `DurableWorkerConfig::approval_timeout` when set) and resumes on the same session.

**Observing and suspending runs.** `DurableWorker::with_hooks` registers `WorkerHooks` whose `on_claim`, `on_park` (with a `ParkReason`), and `on_finish` run after each store write the worker makes. `WorkerHandle::suspend(run_id)` stops an agent run in flight on that worker at its next model-call boundary and parks it `Waiting` on its `Session` checkpoint; a `wake` makes it claimable, and the claim resumes the session on any worker without executing a completed call again (see [Durable Runtime](../docs/official_docs/core/durable-runtime.md#suspending-a-run)).

**Whose identity a run uses.** The worker acts as the runner's own principal, for the run's user. A run's stored principal is honoured only for agent ids listed with `DurableWorkerConfig::with_allowed_principal` (none by default); any other fails the run with `identity.principal_not_allowed`, recorded on the audit trail. Runs from the scheduler, webhook hooks, and queue ingress act for their user as a service (see [Agent Identity](../docs/official_docs/security/agent-identity.md#durable-runs)).

## Webhook Wake-ups (feature: `webhook-ingress`)

`ServerBuilder::with_webhook_ingress` serves `POST /hooks/{name}`. Each `WebhookHook` verifies Standard Webhooks signatures and maps the delivery to a `NewRun` in a `DurableRunStore`, keyed by the `webhook-id`:

| Status | Meaning |
|--------|---------|
| `202` | New run enqueued: `{"runId", "state", "duplicate": false}` |
| `200` | Redelivery of a `webhook-id` that already has a run; the same run is returned |
| `401` | Signature missing, invalid, or outside the timestamp tolerance |
| `409` | `OnConflict::Reject` and an unfinished run holds the hook's concurrency key |
| `422` | Body is not JSON, or a key template names a missing field |
| `429` | The hook's `max_pending` cap is reached (`Retry-After`) |
| `503` | The kill switch is frozen or stale, or the run store is unreachable (`Retry-After`) |

Share the run store with `BackgroundState::with_run_store` and the embedded `DurableWorker` runs woken runs like any other. With a mirrored `GovernanceControl` as `ServerConfig::governance`, a freeze written by any replica refuses deliveries on every replica. See [Wake-ups and Webhooks](../docs/official_docs/core/wake-ups.md).

## Approvals (feature: `approvals`)

`ServerBuilder::with_approval_service(service)` mounts the approval inbox behind the auth middleware; every route needs an authenticated caller, who is the approver.

| Route | Effect |
|-------|--------|
| `GET /api/approvals` | The caller's inbox (`state`, `group`, `app`, `user`, `session`, `cursor`, `limit`); `session` needs `app` and names a session of `user`, the caller by default |
| `GET /api/approvals/{id}` | One request |
| `POST /api/approvals/{id}/decide` | `approve`, `edit`, `reject`, or `respond` with `expectedVersion` |
| `POST /api/approvals/{id}/cancel` | Cancel a request |
| `POST /api/approvals/delegations` | Delegate the caller's authority for a window |
| `DELETE /api/approvals/delegations/{id}` | Revoke a delegation |

A stale `expectedVersion` returns `409` (`approval.version_conflict`). `spawn_approval_ticker` runs expiry and escalation, and `WebhookApprovalNotifier` sends Standard Webhooks-signed notifications. See [Approval Service](../docs/official_docs/security/approval-service.md).

## AWP Agent Edge (feature: `awp`)

`ServerBuilder::awp(edge)` mounts an `AwpEdge` through `add_root_routes`, so the host guard, CORS, timeouts and security headers apply, and attaches the server's kill switch to it. The edge serves one site's capabilities (spec 3.5 §7.9):

| Route | Purpose |
|-------|---------|
| `GET /.well-known/agent-card.json` | The signed A2A card, one skill per capability the caller's tier sees |
| `POST /awp/v2/a2a`, `/awp/v2/a2a/v1/…` | A2A v1.0 JSON-RPC and HTTP+JSON: capabilities and conversation with the public agent |
| `/mcp`, `/.well-known/mcp/server-card.json` | The capabilities as MCP tools |
| `GET /awp/v2/manifest`, `/awp/v2/status`, `/awp/v2/trust` | Contract surfaces |
| `POST /awp/v2/capabilities/{name}`, `GET /awp/v2/calls/{id}` | Invocation (JSON or form-encoded) and held-call status |
| `GET /awp/v2/chat` | The on-site assistant over a WebSocket |

Tiers come only from verified credentials (`WebBotAuthTrustAssigner`, `OAuthClientCredentials`, `MtlsCredentials`, `ApiKeyCredentials`, owner rules). Every binding enters `GovernedExecutor`: `adk_core::authorize_tool_call` under a policy compiled from the manifest, the owner's `ApprovalService` for held calls (`202` with `Location`), the action ledger and a full-body idempotency record. `EdgeStatusControl` switches the edge to read-only and back. Subscriptions get Standard Webhooks deliveries with retries, suspension and replay; consent receipts are signed and revocable by their subject; the beacon counts without cookies or identifiers. See [The AWP Agent Edge](../docs/official_docs/deployment/awp-edge.md).

## API Endpoints

Every endpoint below is nested under `/api`.

### Health and Apps

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/health` | GET | Health check with component status |
| `/api/apps` | GET | List available agents |
| `/api/list-apps` | GET | adk-go compatible app listing |

### Sessions

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/sessions` | POST | Create a session (`appName`, `userId`, optional `sessionId`) |
| `/api/sessions/{app_name}/{user_id}/{session_id}` | GET, DELETE | Get or delete a session |
| `/api/apps/{app_name}/users/{user_id}/sessions` | GET, POST | List or create sessions |
| `/api/apps/{app_name}/users/{user_id}/sessions/{session_id}` | GET, POST, DELETE | Get, create, or delete a session |

Session errors map to their status: `404` not found, `409` already exists, `400` invalid identifiers, `503` unavailable backend.

### Runtime

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/run/{app_name}/{user_id}/{session_id}` | POST | Run the agent on an existing session, streaming SSE (`new_message`, optional `attachments`) |
| `/api/run_sse` | POST | adk-go compatible SSE runtime |
| `/api/run` | POST | Run to completion and return every event as a JSON array |

`/api/run_sse` and `/api/run` create the session only when it does not exist; any other read failure fails the request.

### Artifacts

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/sessions/{app_name}/{user_id}/{session_id}/artifacts` | GET | List artifacts for a session |
| `/api/sessions/{app_name}/{user_id}/{session_id}/artifacts/{artifact_name}` | GET | Get a specific artifact |

### Debug and Tracing

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/debug/trace/{event_id}` | GET | Trace by event ID; mounted without an auth bridge, or with `expose_admin_debug` |
| `/api/debug/trace/session/{session_id}` | GET | All spans for a session |
| `/api/debug/graph/{app_name}/{user_id}/{session_id}/{event_id}` | GET | Graph visualization |
| `/api/apps/{app_name}/users/{user_id}/sessions/{session_id}/events/{event_id}` | GET | Event data |
| `/api/apps/{app_name}/users/{user_id}/sessions/{session_id}/events/{event_id}/graph` | GET | Graph (path-style) |
| `/api/apps/{app_name}/eval_sets` | GET | Evaluation sets (stub) |

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
- `ag_ui` is a hybrid subset: the default stream remains the generic ADK wrapper, but clients can opt into `protocol_native` transport plus AG-UI run input fields on `/api/run_sse`. With `with_approval_service`, a protocol-native run ends on held calls with AG-UI `tool_call` interrupts naming their inbox entries, and the next run's `resume` decides them through the approval service, reporting each answer as a `CUSTOM` `adk.decision` event and a refused one as `RUN_ERROR` (spec 3.3 §10 S2; see [UI Tools](../docs/official_docs/tools/ui-tools.md#ag-ui-decisions)).
- A host adds to protocol-native `ag_ui` runs through the AG-UI extension point (`rest::controllers::AgUiExtension`, set with `ServerBuilder::with_ag_ui_extension`): extra events after each event's own, which client-sent `activity` messages are kept, and a forwarded action held as a requested call so the run ends with a `tool_call` interrupt; its features join `ag_ui` in `GET /api/ui/capabilities`. `adk-surface`'s `GenuiAgUi` uses it for `a2ui-surface` activities (spec 3.3 §6.1 `genui`; see [Agent-Drawn UI](../docs/official_docs/surfaces/genui.md#over-ag-ui)).
- `mcp_apps` is a compatibility subset with `ui://` resource registration plus additive `initialize` / `message` / `update-model-context` bridge helpers, notification polling, list-changed host flows, and runtime request fields, not a browser `postMessage` host bridge; `adk-surface`'s `genui-apps` feature is the governed MCP Apps host (see [Apps from MCP servers](../docs/official_docs/surfaces/genui.md#apps-from-mcp-servers)).

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

### The web home

`adk-server` serves no page of its own (spec 3.3 §10 S1). The web home — Threads, Today and
Memory & rules, and a developer panel with the topology, telemetry and state the `/api/ui/*`
and `/api/debug/*` routes report — is `adk-surface`'s `web` feature, mounted with
`ServerBuilder::add_api_routes(adk_surface::web::router(engine))` and
`add_root_routes(adk_surface::web::home())` behind a `RequestContextExtractor`. Until 3.0 the
server bundled a runtime UI at `/`, `/ui/` and `/ui/assets/config/runtime-config.json`; those
paths now answer `404`. See
[The Web Home](../docs/official_docs/surfaces/web-home.md).

## Security

The server applies these layers to every route:

| Layer | Default |
|-------|---------|
| Host check | `localhost`, `*.localhost`, IP addresses, the A2A and backend URL hosts, and `allowed_hosts`; others get `403` |
| Origin check and CORS | No cross-origin access; `allowed_origins` admits origins |
| Request body limit | 10 MB (`max_body_size`) |
| Request timeout | 30 seconds (`request_timeout`; `SecurityConfig::development()` uses 60) |
| Agent run timeout | 15 minutes for `/api/run`, `/api/run/{app}/{user}/{session}`, `/api/run_sse`, `/a2a`, and `/a2a/stream` (`run_timeout`, `ServerConfig::with_run_timeout`; `DEFAULT_RUN_TIMEOUT`) |
| Response headers | `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`, `X-XSS-Protection: 1; mode=block` |
| Request ID | `x-request-id` |
| Error details | A2A internal errors read `Internal server error` (`expose_error_details: false`); REST errors carry their code, category, and message |
| User authorization | Session, artifact, and debug endpoints enforce the authenticated user when an auth bridge is configured |

## Changes in 3.0

| Change | Migration |
|--------|-----------|
| The server answers loopback hosts only and allows no cross-origin access by default | List public hosts with `with_allowed_hosts` and origins with `with_allowed_origins` (`vec!["*".to_string()]` restores open CORS) |
| `SecurityConfig` gains `allowed_hosts`; `ServerConfig` gains `governance` | Use `ServerConfig::new` or add the fields to struct literals |
| `SecurityConfig::with_allowed_hosts` and `ServerConfig::with_allowed_hosts` both replace the host list | Pass every host in one call |
| `SecurityConfig::development()` admits loopback origins on any port instead of every origin | — |
| A2A clients send messages only to the origin the agent card came from (`a2a.card_endpoint_refused`) | `allow_endpoint_origin` or `resolve_agent_card_allowing` |
| Model thinking never reaches an A2A peer; `adk_part_to_wire` returns `Result<Option<Part>>` | — |
| Background runs and cron run on `DurableRunStore` and `ScheduleStore`; `RunStore`, `CronJobStore`, and the file persistence are removed | See the changelog for the removed types and the new REST fields |
| `DurableWorker` refuses untrusted stored principals (`identity.principal_not_allowed`) | List them with `DurableWorkerConfig::with_allowed_principal` |
| A failed session lookup no longer recreates the session | — |
| Webhook ingress, approvals endpoints, and governance endpoints are new | — |

See the [2.x to 3.0 migration guide](../docs/official_docs/migration/2.x-to-3.0.md) and the [changelog](../CHANGELOG.md) for the full list.

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) — umbrella crate
- [adk-runner](https://crates.io/crates/adk-runner) — execution runtime
- [adk-cli](https://crates.io/crates/adk-cli) — command-line launcher
- [adk-telemetry](https://crates.io/crates/adk-telemetry) — OpenTelemetry integration
- [adk-artifact](https://crates.io/crates/adk-artifact) — artifact storage
- [adk-auth](https://crates.io/crates/adk-auth) — authentication, including the JWT auth bridge
- [adk-surface](https://crates.io/crates/adk-surface) — the web home and agent-drawn UI (`genui`), which replaces `adk-ui`

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
