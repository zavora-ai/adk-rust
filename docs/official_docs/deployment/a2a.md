# Agent-to-Agent (A2A) Protocol

ADK-Rust implements the [A2A Protocol v1.0.0](https://google.github.io/A2A/) for cross-network agent communication. The implementation lives in `adk-server` behind the `a2a-v1` feature flag and covers all 11 JSON-RPC operations, REST bindings, agent card discovery, and version negotiation. Wire types are provided by [`a2a-protocol-types`](https://crates.io/crates/a2a-protocol-types) — the Foundation-verified Rust A2A SDK by [@tomtom215](https://github.com/tomtom215) ([a2a-rust](https://github.com/tomtom215/a2a-rust)).

## Overview

A2A is useful when:
- Integrating with third-party agent services
- Building microservices architectures with specialized agents
- Enabling cross-language agent communication (any language with an A2A client)
- Enforcing formal contracts between agent systems

For simple internal organization, use local sub-agents instead of A2A for better performance.

## v1.0.0 Compliance

The implementation is fully compliant with the A2A Protocol v1.0.0 specification:

| Feature | Spec Section | Status |
|---------|-------------|--------|
| Agent card with capabilities declaration | §8 | ✅ |
| RFC 3339 timestamps on all task status changes | §5.6.1 | ✅ |
| Message ID idempotency for `SendMessage` | §3.3.1 | ✅ |
| Push notification authentication (Bearer + token) | §13.2 | ✅ |
| INPUT_REQUIRED multi-turn resume flow | §3.4.3 | ✅ |
| Input validation (parts, IDs, metadata size) | §3.3 | ✅ |
| `Content-Type: application/a2a+json` on responses | §9 | ✅ |
| Task object as first SSE streaming event | §3.1.2 | ✅ |
| Context-scoped task lookup for multi-turn | §3.4.1 | ✅ |
| Version negotiation (`A2A-Version` header) | §9.1 | ✅ |
| State machine validation (terminal states) | §4.1.3 | ✅ |

## Agent Cards

Every A2A agent exposes an agent card at `/.well-known/agent-card.json` describing its capabilities, skills, and supported interfaces.

```rust
use adk_server::a2a::v1::card::build_v1_agent_card;
use a2a_protocol_types::{AgentCapabilities, AgentSkill};

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

The agent card includes:
- Agent name, description, and version
- Supported interfaces with protocol binding and version
- Capabilities: `streaming`, `pushNotifications`, `extendedAgentCard`
- Skills derived from the agent configuration
- Default input/output modes

Capabilities are now explicitly declared via the `AgentCapabilities` parameter — no more hardcoded defaults.

## Exposing an Agent via A2A v1

Build a full A2A v1.0.0 server with LLM integration:

```rust
use std::sync::Arc;
use a2a_protocol_types::{AgentCapabilities, AgentSkill};
use adk_agent::LlmAgentBuilder;
use adk_server::a2a::v1::card::{CachedAgentCard, build_v1_agent_card};
use adk_server::a2a::v1::executor::V1Executor;
use adk_server::a2a::v1::jsonrpc_handler::jsonrpc_handler;
use adk_server::a2a::v1::push::NoOpPushNotificationSender;
use adk_server::a2a::v1::request_handler::RequestHandler;
use adk_server::a2a::v1::rest_handler::rest_router;
use adk_server::a2a::v1::task_store::InMemoryTaskStore;
use adk_server::a2a::v1::version::version_negotiation;
use adk_runner::RunnerConfig;
use adk_session::InMemorySessionService;
use axum::Router;
use axum::routing::post;
use tokio::sync::RwLock;

// 1. Create your agent
let model = adk_model::GeminiModel::new(&api_key, "gemini-2.5-flash")?;
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
    "my-agent", "A helpful agent",
    "http://localhost:3001/jsonrpc", "1.0.0",
    vec![/* skills */],
    AgentCapabilities::none().with_streaming(true),
);
let cached_card = Arc::new(RwLock::new(CachedAgentCard::new(card)));

// 4. Create runner config for LLM invocation
let session_service = Arc::new(InMemorySessionService::new());
let runner_config = Arc::new(RunnerConfig {
    app_name: "my-agent".to_string(),
    agent: Arc::new(agent),
    session_service,
    artifact_service: None,
    memory_service: None,
    plugin_manager: None,
    run_config: None,
    compaction_config: None,
    context_cache_config: None,
    cache_capable: None,
    request_context: None,
    cancellation_token: None,
});

// 5. Wire up the handler and routes
let handler = Arc::new(RequestHandler::with_runner(
    executor, task_store, push_sender, cached_card, runner_config,
));

let app = Router::new()
    .route("/jsonrpc", post(jsonrpc_handler))
    .with_state(handler.clone())
    .merge(rest_router(handler))
    .layer(axum::middleware::from_fn(version_negotiation));

// 6. Serve
let listener = tokio::net::TcpListener::bind("0.0.0.0:3001").await?;
axum::serve(listener, app).await?;
```

This exposes:
- `GET /.well-known/agent-card.json` — Agent card with ETag caching
- `POST /jsonrpc` — JSON-RPC endpoint (all 11 v1 operations)
- REST routes for all operations
- `A2A-Version` header negotiation on all routes

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

Duplicate `SendMessage` requests with the same `messageId` return the previously created task without re-processing. This applies to both `SendMessage` and `SendStreamingMessage`.

## Push Notification Authentication

When a client registers a webhook via `CreateTaskPushNotificationConfig`, the server includes authentication headers on webhook deliveries:

- `Authorization: Bearer <credentials>` — when `authentication` field has bearer credentials
- `a2a-notification-token: <token>` — when `token` field is present

Both headers can be set simultaneously. SSRF protection validates webhook URLs against private IP ranges and localhost.

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

Use `RemoteA2aAgent` to communicate with a remote A2A agent:

```rust
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

For direct protocol-level communication:

```rust
use adk_server::a2a::client::v1_client::A2aV1Client;

// Discover agent card
let card = A2aV1Client::resolve_agent_card("http://localhost:3001").await?;
let client = A2aV1Client::new(card);

// Send message
let task = client.send_message(message).await?;

// Get task
let task = client.get_task(&task_id, Some(10)).await?;

// List tasks
let tasks = client.list_tasks(None, None, None, None).await?;

// Cancel task
client.cancel_task(&task_id).await?;

// Streaming
let response = client.send_streaming_message(message).await?;

// Push notification CRUD
let config = client.create_push_notification_config(config).await?;
client.delete_push_notification_config(&task_id, &config_id).await?;
```

## Error Handling

A2A errors map to both JSON-RPC codes and HTTP status codes:

| Error | JSON-RPC Code | HTTP Status |
|-------|--------------|-------------|
| TaskNotFound | -32001 | 404 |
| TaskNotCancelable | -32002 | 409 |
| PushNotificationNotSupported | -32003 | 400 |
| UnsupportedOperation | -32004 | 400 |
| ContentTypeNotSupported | -32005 | 415 |
| InvalidAgentResponse | -32006 | 502 |
| VersionNotSupported | -32009 | 400 |
| InvalidParams | -32602 | 400 |
| MethodNotFound | -32601 | 404 |
| Internal | -32603 | 500 |

## Running the Examples

Two complete A2A v1.0.0 example agents are included:

```bash
# Terminal 1: Start the research agent (port 3001)
cd examples/a2a-research-agent
cp .env.example .env  # add GOOGLE_API_KEY or OPENAI_API_KEY
cargo run

# Terminal 2: Start the writing agent (port 3002)
cd examples/a2a-writing-agent
cp .env.example .env
cargo run --bin a2a-writing-agent

# Terminal 3: Run the client that exercises all 11 operations
cd examples/a2a-writing-agent
cargo run --bin client
```

The client validates: agent card discovery, SendMessage (both agents with real LLM), GetTask, ListTasks, CancelTask error path, SendStreamingMessage, push notification CRUD, GetExtendedAgentCard, version negotiation, and error paths.

## Best Practices

1. **Declare capabilities accurately** — set `streaming`, `pushNotifications` based on what your agent actually supports
2. **Use streaming for long operations** — `SendStreamingMessage` gives clients real-time progress
3. **Handle multi-turn flows** — use `contextId` to maintain conversation state across messages
4. **Validate webhook URLs** — SSRF protection is built-in, but use HTTPS in production
5. **Set appropriate timeouts** — configure request timeouts for remote agent calls
6. **Use idempotency** — clients can safely retry `SendMessage` with the same `messageId`

## Related

- [LlmAgent](../agents/llm-agent.md) — Creating agents
- [Multi-Agent Systems](../agents/multi-agent.md) — Sub-agents and hierarchies
- [Server Deployment](server.md) — Running agents as HTTP servers

---

**Previous**: [← Server](server.md) | **Next**: [Evaluation →](../evaluation/evaluation.md)
