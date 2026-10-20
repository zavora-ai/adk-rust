# Agent-to-Agent (A2A) Protocol

ADK-Rust implements the [A2A Protocol v1.0.0](https://google.github.io/A2A/) for cross-network agent communication. The implementation lives in `adk-server` behind the `a2a-v1` feature flag and covers all 11 JSON-RPC operations, REST bindings, agent card discovery, and version negotiation. See [Operation coverage](#operation-coverage) for the one operation whose semantics are narrower than the specification allows. Wire types are provided by [`a2a-protocol-types`](https://crates.io/crates/a2a-protocol-types) — the Rust A2A SDK by [@tomtom215](https://github.com/tomtom215) ([a2a-rust](https://github.com/tomtom215/a2a-rust)).

## Overview

A2A is useful when:
- Integrating with third-party agent services
- Building microservices architectures with specialized agents
- Enabling cross-language agent communication (any language with an A2A client)
- Enforcing formal contracts between agent systems

For simple internal organization, use local sub-agents instead of A2A for better performance.

## v1.0.0 Coverage

The v1 implementation provides:

| Feature |
|---------|
| Agent card with capabilities declaration |
| RFC 3339 timestamps on task status changes |
| Message ID idempotency for `SendMessage` |
| Push notification authentication (Bearer + token) |
| `INPUT_REQUIRED` multi-turn resume flow |
| Input validation (parts, IDs, metadata size) |
| `Content-Type: application/a2a+json` on responses |
| Task object as first SSE streaming event |
| Context-scoped task lookup for multi-turn |
| Version negotiation (`A2A-Version` header) |
| State machine validation (terminal states) |

`SubscribeToTask` returns a snapshot rather than a live stream; see
[Operation coverage](#operation-coverage).

## Two Server Surfaces

`adk-server` serves A2A in two ways:

| Surface | Mount it with | Routes | Agent card |
|---------|---------------|--------|------------|
| v1.0.0 (`a2a::v1`, `a2a-v1` feature) | your own Axum router, as below | `POST /jsonrpc` (or the path you choose), REST bindings | the route you add; `protocolVersion: "1.0"` in `supportedInterfaces` |
| Pre-v1 (A2A 0.3) | `ServerBuilder::with_a2a(base_url)`, `create_app_with_a2a`, `Launcher::with_a2a_base_url` | `POST /a2a`, `POST /a2a/stream` (`message/send`, `message/stream`) | served at `/.well-known/agent-card.json` and `/.well-known/agent.json` |

The pre-v1 card advertises no `supportedInterfaces`, so `A2aV1Client` resolves it but has no
endpoint to send requests to; use `A2aClient` or `RemoteA2aAgent` with a pre-v1 server. The
`adk-rust` `server` feature enables `a2a-v1`.

## Agent Cards

A v1 agent card describes the agent's capabilities, skills, and supported interfaces.

```rust
use a2a_protocol_types::{AgentCapabilities, AgentSkill};
use adk_server::a2a::v1::card::build_v1_agent_card;

let card = build_v1_agent_card(
    "my-agent",
    "A helpful research agent",
    "http://localhost:3001/jsonrpc",
    "1.0.0",
    vec![AgentSkill {
        id: "research".to_string(),
        name: "Research & Summarize".to_string(),
        description: "Researches topics and produces structured summaries".to_string(),
        tags: vec!["research".to_string()],
        examples: None,
        input_modes: None,
        output_modes: None,
        security_requirements: None,
    }],
    AgentCapabilities::none()
        .with_streaming(true)
        .with_push_notifications(true),
);
```

`build_v1_agent_card` sets:
- Agent name, description, and version
- One supported interface: the JSON-RPC binding at the given URL, protocol version `1.0`
- The capabilities you pass: `streaming`, `pushNotifications`, `extendedAgentCard`
- The skills you pass
- Default input and output modes `["text/plain"]`

## Exposing an Agent via A2A v1

Build an A2A v1.0.0 server with LLM integration:

```rust
use a2a_protocol_types::AgentCapabilities;
use adk_agent::LlmAgentBuilder;
use adk_runner::Runner;
use adk_server::a2a::v1::card::{CachedAgentCard, build_v1_agent_card};
use adk_server::a2a::v1::executor::V1Executor;
use adk_server::a2a::v1::jsonrpc_handler::jsonrpc_handler;
use adk_server::a2a::v1::push::NoOpPushNotificationSender;
use adk_server::a2a::v1::request_handler::RequestHandler;
use adk_server::a2a::v1::rest_handler::rest_router;
use adk_server::a2a::v1::task_store::InMemoryTaskStore;
use adk_server::a2a::v1::version::version_negotiation;
use adk_session::InMemorySessionService;
use axum::Router;
use axum::http::header;
use axum::routing::{get, post};
use std::sync::Arc;
use tokio::sync::RwLock;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;

    // 1. Create your agent
    let model = adk_model::GeminiModel::new(&api_key, "gemini-3.7-flash")?;
    let agent = LlmAgentBuilder::new("my-agent")
        .description("A helpful agent")
        .model(Arc::new(model))
        .instruction("You are a helpful assistant.")
        .build()?;

    // 2. Set up A2A infrastructure
    let task_store = Arc::new(InMemoryTaskStore::new());
    let executor = Arc::new(V1Executor::new(task_store.clone()));
    let push_sender = Arc::new(NoOpPushNotificationSender);

    // 3. Build agent card with capabilities
    let card = build_v1_agent_card(
        "my-agent",
        "A helpful agent",
        "http://localhost:3001/jsonrpc",
        "1.0.0",
        vec![],
        AgentCapabilities::none().with_streaming(true),
    );
    let cached_card = Arc::new(RwLock::new(CachedAgentCard::new(card)));

    // 4. Create the runner configuration for LLM invocation
    let runner_config = Arc::new(
        Runner::builder()
            .app_name("my-agent")
            .agent(Arc::new(agent))
            .session_service(Arc::new(InMemorySessionService::new()))
            .build_config(),
    );

    // 5. Wire up the handler and routes
    let handler = Arc::new(RequestHandler::with_runner(
        executor,
        task_store,
        push_sender,
        cached_card.clone(),
        runner_config,
    ));

    let card_route = Router::new().route(
        "/.well-known/agent-card.json",
        get(move || {
            let cached_card = cached_card.clone();
            async move {
                let card = cached_card.read().await;
                (
                    [
                        (header::CONTENT_TYPE, "application/json".to_string()),
                        (header::ETAG, format!("\"{}\"", card.etag)),
                    ],
                    card.card_json.clone(),
                )
            }
        }),
    );

    let app = Router::new()
        .route("/jsonrpc", post(jsonrpc_handler))
        .with_state(handler.clone())
        .merge(rest_router(handler))
        .layer(axum::middleware::from_fn(version_negotiation))
        .merge(card_route);

    // 6. Serve
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3001").await?;
    axum::serve(listener, app).await?;
    Ok(())
}
```

A router served this way carries no host or origin checks and no authentication. Mounted with
`ServerBuilder::add_api_routes`, it is served under `/api` (`POST /api/jsonrpc`) behind the
server's [host and origin defaults](server.md#host-origin-and-bind-defaults) and its
`RequestContextExtractor`, so each operation runs for the authenticated caller.
`ServerBuilder::add_root_routes` applies the host and origin checks but not the
`RequestContextExtractor`: every request through a root route acts as an unauthenticated caller.

This exposes:
- `POST /jsonrpc` — JSON-RPC endpoint (all 11 v1 operations; see [Operation coverage](#operation-coverage))
- REST routes for the same operations (`/message:send`, `/message:stream`, `/tasks`, `/tasks/{taskId}`,
  `/tasks/{taskId}/cancel`, `/tasks/{taskId}/subscribe`, `/tasks/{taskId}/pushNotificationConfigs`,
  `/extendedAgentCard`)
- `A2A-Version` header negotiation on the JSON-RPC and REST routes
- `GET /.well-known/agent-card.json` — the card, with an `ETag` (`CachedAgentCard::matches_etag`
  answers `If-None-Match`; the `a2a-research-agent` example returns `304` with it)

## JSON-RPC Operations

All 11 A2A v1.0.0 operations are supported:

| Method | Description |
|--------|-------------|
| `SendMessage` | Send a message, create/resume a task |
| `SendStreamingMessage` | Same as SendMessage but returns SSE stream |
| `GetTask` | Retrieve a task by ID |
| `CancelTask` | Cancel a running task |
| `ListTasks` | List tasks with filtering and pagination |
| `SubscribeToTask` | Subscribe to task updates via SSE |
| `CreateTaskPushNotificationConfig` | Register a webhook for push notifications |
| `GetTaskPushNotificationConfig` | Retrieve a push notification config |
| `ListTaskPushNotificationConfigs` | List push configs for a task |
| `DeleteTaskPushNotificationConfig` | Remove a push notification config |
| `GetExtendedAgentCard` | Retrieve the extended agent card |

### SendMessage

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "SendMessage",
  "params": {
    "message": {
      "messageId": "msg-123",
      "role": "ROLE_USER",
      "parts": [{"text": "Research quantum computing"}]
    }
  }
}
```

Response includes a Task object with status, history, and artifacts. The response uses `Content-Type: application/a2a+json`.

### SendStreamingMessage

Same request format as SendMessage. Returns an SSE stream where:
1. First event is a complete `Task` object (per spec §3.1.2)
2. Subsequent events are `TaskStatusUpdateEvent` (Working, Completed, etc.)
3. Artifact events are `TaskArtifactUpdateEvent`

### Multi-Turn Conversations

When a task reaches `INPUT_REQUIRED` state, send a follow-up message with the same `contextId` to resume it:

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "SendMessage",
  "params": {
    "message": {
      "messageId": "msg-456",
      "role": "ROLE_USER",
      "contextId": "ctx-original",
      "parts": [{"text": "Yes, include more details on error correction"}]
    }
  }
}
```

The handler automatically finds the existing task by `contextId`, transitions it from `INPUT_REQUIRED` to `Working`, appends the new message to history, and continues processing.

### Idempotency

Duplicate `SendMessage` requests with the same `messageId` from the same caller return the previously created task without re-processing. This applies to both `SendMessage` and `SendStreamingMessage`.

### Caller Identity

Every operation runs for the authenticated caller of the request. `jsonrpc_handler` and `rest_router` read the caller with the `AuthenticatedCaller` extractor, which accepts an `Option<RequestContext>` or a bare `RequestContext` request extension. Mount them behind the middleware that authenticates callers and inserts that extension.

| | Authenticated caller | No authentication |
|---|---|---|
| Session user | the caller's `user_id` | `a2a-{contextId}` |
| Task owner | the caller's `user_id` | none |
| Tasks visible to `GetTask`, `CancelTask`, `ListTasks`, `SubscribeToTask`, push-config operations | tasks the caller created | ownerless tasks |
| `INPUT_REQUIRED` resume by `contextId` | the caller's own task only | ownerless tasks only |

A task owned by someone else answers `TaskNotFound`, the same as a task that does not exist. The owner is kept in task metadata under `adk.owner` (`OWNER_METADATA_KEY`) and is never sent on the wire. Programmatic callers use `RequestHandler::for_caller(Some(request_context))`; the `RequestHandler` operation methods act for an unauthenticated caller.

> **Important:** without authentication, the session user is derived from the client-supplied `contextId`, so any client that knows a `contextId` shares that session. Configure authentication for any deployment with more than one client.

The pre-v1 `/a2a` and `/a2a/stream` routes mounted by `create_app_with_a2a` and `ServerBuilder::with_a2a` apply the same rules, with `A2A_USER_{contextId}` as the unauthenticated session user. They take the caller from the server's `RequestContextExtractor`. Finished task records there are bounded by `A2aTaskRetention` (newest 1,000 for at most one hour by default; configure with `ServerBuilder::with_a2a_task_retention`), and a `message/send` that reuses the `taskId` of a running task, or of another caller's task, is rejected with JSON-RPC error `-32602`.

## Push Notifications

`RequestHandler` implements the four push notification config operations: it stores each config
on its task and returns it from `Get`/`List`, and `Delete` removes it. It does not send
notifications, and it does not validate the registered URL: the `PushNotificationSender` passed to
`RequestHandler::new` or `with_runner` is not called by the handler. Delivery is available through
`HttpPushNotificationSender`, whose `send_status_update` and `send_artifact_update` post to a
config's URL with authentication headers:

- `Authorization: Bearer <credentials>` — when the `authentication` field has bearer credentials
- `a2a-notification-token: <token>` — when the `token` field is present

Both headers can be set simultaneously. A failed delivery is retried with backoff.

### Webhook URL Validation

`HttpPushNotificationSender` checks the URL with `validate_webhook_url` before every delivery.
`validate_webhook_url` rejects a webhook whose host is `localhost`, a `*.localhost` name, or a literal address in a non-public range:

| Range | Purpose |
|-------|---------|
| `0.0.0.0/8`, `::` | unspecified |
| `127.0.0.0/8`, `::1` | loopback |
| `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16` | private |
| `100.64.0.0/10` | carrier-grade NAT |
| `169.254.0.0/16`, `fe80::/10` | link-local, including cloud metadata endpoints |
| `fc00::/7`, `fec0::/10` | IPv6 unique-local and site-local |
| `224.0.0.0/4`, `255.255.255.255`, `ff00::/8` | multicast and broadcast |
| IPv4-mapped, IPv4-compatible, and NAT64 IPv6 | any IPv4 range above |

`HttpPushNotificationSender::new()` does not follow redirects and bounds each delivery attempt at 10 seconds (5 seconds to connect). A client passed to `HttpPushNotificationSender::with_client` should do the same, since validation covers the registered URL only. Host names are not resolved during validation; route deliveries through an egress proxy when webhook hosts are untrusted.

## Input Validation

All incoming requests are validated before processing:

| Validation | Error |
|-----------|-------|
| Message with zero parts | `InvalidParams` (-32602) |
| Empty or whitespace-only messageId | `InvalidParams` (-32602) |
| messageId exceeding 256 characters | `InvalidParams` (-32602) |
| Empty or whitespace-only taskId | `InvalidParams` (-32602) |
| taskId exceeding 256 characters | `InvalidParams` (-32602) |
| Metadata exceeding 64 KB | `InvalidParams` (-32602) |

## Consuming a Remote Agent

`RemoteA2aAgent` runs a remote agent as a local agent. It resolves the card at the agent URL with
`A2aClient` and speaks the pre-v1 `message/send` and `message/stream` methods, so it calls servers
built with `ServerBuilder::with_a2a`. For a v1 server, build an
`adk_server::a2a::remote_agent::v1_remote::RemoteA2aV1Agent` from a card resolved with
`A2aV1Client::resolve_agent_card`.

```rust
use adk_rust::prelude::*;
use adk_server::a2a::RemoteA2aAgent;

let remote_agent = RemoteA2aAgent::builder("prime_checker")
    .description("Checks if numbers are prime")
    .agent_url("http://localhost:8001")
    .build()?;

// Use as a sub-agent in a local agent hierarchy
let root_agent = LlmAgentBuilder::new("root")
    .model(Arc::new(model))
    .sub_agent(Arc::new(remote_agent))
    .build()?;
```

## A2A Client

For direct protocol-level communication with a v1 server:

```rust
use a2a_protocol_types::{Message, MessageId, MessageRole, Part, TaskPushNotificationConfig};
use adk_server::a2a::client::v1_client::A2aV1Client;

// Discover the agent card; its interfaces must be on the URL's origin
let card = A2aV1Client::resolve_agent_card("http://localhost:3001").await?;
let client = A2aV1Client::new(card);

let message = Message {
    id: MessageId::new("msg-1"),
    role: MessageRole::User,
    parts: vec![Part::text("Research quantum computing")],
    task_id: None,
    context_id: None,
    reference_task_ids: None,
    extensions: None,
    metadata: None,
};

// Send message
let task = client.send_message(message.clone()).await?;
let task_id = task.id.to_string();

// Get task, with at most 10 history messages
let task = client.get_task(&task_id, Some(10)).await?;

// List tasks (context ID, state, page size, page token)
let tasks = client.list_tasks(None, None, None, None).await?;

// Cancel task
client.cancel_task(&task_id).await?;

// Streaming: the raw SSE response
let response = client.send_streaming_message(message).await?;

// Push notification CRUD
let config = client
    .create_push_notification_config(TaskPushNotificationConfig::new(&task_id, "https://hooks.example.com/a2a"))
    .await?;
client.delete_push_notification_config(&task_id, config.id.as_deref().unwrap_or_default()).await?;
```

Each call returns `V1ClientError` on failure; a server error arrives as its JSON-RPC code and
message.

### Card endpoints

An agent card names the URL messages are sent to, and the card comes from the remote party.
`A2aClient::resolve_agent_card`, `A2aClient::from_url`, `A2aV1Client::resolve_agent_card`, and
`RemoteA2aAgent` accept a card only when its endpoint passes
`adk_server::a2a::client::check_card_endpoint`:

| Endpoint | Accepted |
|----------|----------|
| On the origin the card was fetched from (`localhost`, `127.0.0.1`, and `::1` count as one host) | yes |
| On an origin passed to `resolve_agent_card_allowing` or `RemoteA2aAgentBuilder::allow_endpoint_origin`, over `https` or on loopback | yes |
| Anywhere else, or an allowed origin over plain `http` | no — `a2a.card_endpoint_refused` |

```rust
use adk_server::a2a::RemoteA2aAgent;

let remote = RemoteA2aAgent::builder("billing")
    .agent_url("https://agents.example.com")
    // The card at agents.example.com names its RPC endpoint on rpc.example.com.
    .allow_endpoint_origin("https://rpc.example.com")
    .build()?;
```

A card passed to `A2aClient::new` or `A2aV1Client::new` directly is trusted as given.

### Streams

The clients decode a stream only once a whole SSE line has arrived, so a multi-byte character
split across reads arrives intact. A stream that ends before the task reaches a final status
(`completed`, `failed`, `canceled`, `rejected`, `input-required`, or `auth-required`) ends in an
error: `A2aClient::send_streaming_message` yields an `Err`, and `RemoteA2aAgent` and
`RemoteA2aV1Agent` yield an event with `error_message` set.

## Error Handling

The JSON-RPC endpoint answers HTTP `200` with the error's JSON-RPC code in the body; the REST
routes answer with the HTTP status. Both carry a `google.rpc.ErrorInfo` detail with the error's
`reason`. An unsupported `A2A-Version` is refused by the version middleware before either handler
runs, with HTTP `400` and the REST error body, on the JSON-RPC route as well:

| Error | JSON-RPC Code | HTTP Status |
|-------|--------------|-------------|
| TaskNotFound | -32001 | 404 |
| TaskNotCancelable | -32002 | 409 |
| PushNotificationNotSupported | -32003 | 400 |
| UnsupportedOperation | -32004 | 400 |
| ContentTypeNotSupported | -32005 | 415 |
| InvalidAgentResponse | -32006 | 502 |
| ExtendedAgentCardNotConfigured | -32007 | 400 |
| ExtensionSupportRequired | -32008 | 400 |
| VersionNotSupported | -32009 | 400 |
| InvalidParams | -32602 | 400 |
| MethodNotFound | -32601 | 404 |
| Internal | -32603 | 500 |

## Running the Examples

Two complete A2A v1.0.0 example agents are included. Each uses `gemini-3.7-flash` when
`GOOGLE_API_KEY` is set, and OpenAI when only `OPENAI_API_KEY` is set. The research agent listens
on port 3001 and the writing agent on 3002 (`PORT` overrides either):

```bash
cargo run --manifest-path examples/a2a-research-agent/Cargo.toml
cargo run --manifest-path examples/a2a-writing-agent/Cargo.toml --bin a2a-writing-agent
cargo run --manifest-path examples/a2a-writing-agent/Cargo.toml --bin client
```

The client exercises agent card discovery, SendMessage (both agents with real LLM), GetTask, ListTasks, CancelTask error path, SendStreamingMessage, push notification CRUD, GetExtendedAgentCard, version negotiation, and error paths.

## Best Practices

1. **Declare capabilities accurately** — set `streaming`, `pushNotifications` based on what your agent actually supports
2. **Use streaming for long operations** — `SendStreamingMessage` gives clients real-time progress
3. **Handle multi-turn flows** — use `contextId` to maintain conversation state across messages
4. **Validate webhook URLs** — call `validate_webhook_url` when a config is registered; the handler accepts any URL, and `HttpPushNotificationSender` checks it only at delivery
5. **Set appropriate timeouts** — configure request timeouts for remote agent calls
6. **Use idempotency** — clients can safely retry `SendMessage` with the same `messageId`

## Related

- [LlmAgent](../agents/llm-agent.md) — Creating agents
- [Multi-Agent Systems](../agents/multi-agent.md) — Sub-agents and hierarchies
- [Server Deployment](server.md) — Running agents as HTTP servers

---

**Previous**: [← Server](server.md) | **Next**: [Evaluation →](../evaluation/evaluation.md)

## Operation coverage

All 11 v1 JSON-RPC operations are dispatched and implemented.

| Operation | Status |
|-----------|--------|
| `SendMessage` | Drives the agent, records its output as an artifact |
| `SendStreamingMessage` | Drives the agent, streams artifact chunks as they are produced |
| `GetTask`, `ListTasks` | Full |
| `CancelTask` | Full |
| `SubscribeToTask` (`tasks/resubscribe`) | **Snapshot only** — see below |
| `CreateTaskPushNotificationConfig`, `GetTaskPushNotificationConfig`, `ListTaskPushNotificationConfigs`, `DeleteTaskPushNotificationConfig` | Full |
| `GetExtendedAgentCard` | Full |

### `SubscribeToTask` is a snapshot

The operation returns the task and its current status, then closes the stream. It does **not**
deliver subsequent updates, so a client must not wait on it for progress. A task in a terminal
state cannot be subscribed to: the request fails with `TaskNotCancelable` (`409` over REST).

A live re-attach requires a per-task event queue that outlives the originating request. The
reference implementations get this from their A2A SDKs — adk-python and adk-go both delegate
`tasks/resubscribe` entirely to the SDK's queue manager, and neither implements it in ADK code.
This server is hand-rolled on `a2a-protocol-types`, which provides wire types rather than a
server runtime, so the queue does not exist yet.

Use `SendStreamingMessage` when live updates are required.

### Streaming event contract

`SendStreamingMessage` translates agent events as they arrive:

| Agent event | A2A event |
|-------------|-----------|
| first, before output | `Task`, then `TaskStatusUpdateEvent` — `Working` |
| content, `partial = true` | `TaskArtifactUpdateEvent` — `append`, not last chunk |
| content, `partial = false` | `TaskArtifactUpdateEvent` — final chunk |
| stream ends after a call held for confirmation, or a long-running call | `TaskStatusUpdateEvent` — `InputRequired` |
| stream ends after an `adk_request_credential` call | `TaskStatusUpdateEvent` — `AuthRequired` |
| stream ends | `TaskStatusUpdateEvent` — `Completed` |
| stream errors | `TaskStatusUpdateEvent` — `Failed` |

`SendMessage` and the pre-v1 `message/send` and `message/stream` routes end a task in the same
states (`input-required`, `auth-required`, `completed`). A message on the same `contextId`
resumes an `InputRequired` task. Model thinking (`Part::Thinking`) is never sent to a peer:
only the answer text is.

All chunks of one response share an artifact ID so a client can reassemble them. The joined text
is persisted, so a later `GetTask` returns what was streamed. This matches the contract
adk-python and adk-go implement over their SDKs.
