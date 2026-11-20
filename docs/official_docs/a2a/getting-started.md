# A2A Getting Started

Create and run an A2A (Agent-to-Agent) protocol agent in under 5 minutes.

## Prerequisites

- Rust 1.95.0 or later (`rustup update stable`)
- `cargo-adk` installed (`cargo install cargo-adk`)
- A Google API key ([get one here](https://aistudio.google.com/app/apikey))

## Scaffold an A2A Project

The fastest way to get started is the `a2a` template:

```bash
cargo adk new my-a2a-agent --template a2a
cd my-a2a-agent
```

This generates a complete project with:
- `Cargo.toml` — `adk-rust` with the `gemini`, `minimal`, `server`, and `sessions` features
  (`server` serves A2A v1 and A2A 0.3)
- `src/main.rs` — A2A server using the `A2aServer` builder
- `.env.example` — API key placeholder
- `README.md`

Add your API key:

```bash
cp .env.example .env
# Edit .env and set GOOGLE_API_KEY=your-key-here
```

Run:

```bash
cargo run
```

Your A2A agent is now serving on `http://127.0.0.1:8080`. The generated server reads `PORT`
(default `8080`) and `BIND_HOST` (default `127.0.0.1`); set `BIND_HOST=0.0.0.0` to listen on
every interface after configuring authentication.

### Other Providers

```bash
# OpenAI
cargo adk new my-agent --template a2a --provider openai

# Anthropic
cargo adk new my-agent --template a2a --provider anthropic
```

---

## Convenience API

ADK-Rust provides `A2aServer` for exposing any agent via the A2A protocol without manual route configuration.

### Zero-Config: `quick_start`

The simplest approach — one function call, sensible defaults:

```rust
use adk_rust::prelude::*;
use adk_rust::server::A2aServer;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;

    let model = GeminiModel::new(&api_key, "gemini-3.8-flash")?;

    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("my-agent")
            .description("A helpful AI assistant")
            .instruction("You are a helpful assistant exposed via A2A.")
            .model(Arc::new(model))
            .build()?,
    );

    let app = A2aServer::quick_start(agent);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;
    axum::serve(listener, app).await?;
    Ok(())
}
```

`quick_start` configures:
- In-memory session service
- A2A v1: the agent card at `GET /.well-known/agent-card.json`, the JSON-RPC binding at
  `POST /a2a/v1`, and the HTTP+JSON binding under `/a2a/v1/rest`
- A2A 0.3: the agent card at `GET /.well-known/agent.json`, and JSON-RPC at `POST /a2a` and
  `POST /a2a/stream`
- The server's REST API, with the [host and origin defaults](../deployment/server.md#host-origin-and-bind-defaults)

The v1 card lists all three endpoints in `supportedInterfaces`, under
`http://localhost:8080`. A2A v1 comes from `adk-server`'s `a2a-v1` feature, which `adk-rust`'s
`server` feature turns on; `adk-server` used without it serves only A2A 0.3, with its card at
both paths.

It returns the router; bind it to `127.0.0.1:8080` so the cards' URLs match the listener.

### Custom Config: Builder

Use the builder when you need control over the address, the card URL, the session backend, or
the security settings:

```rust
use adk_rust::prelude::*;
use adk_rust::server::A2aServer;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(&api_key, "gemini-3.8-flash")?;

    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("my-agent")
            .description("Production A2A agent")
            .instruction("You are a helpful assistant.")
            .model(Arc::new(model))
            .build()?,
    );

    let server = A2aServer::builder()
        .agent(agent)
        .bind_addr("127.0.0.1:9090")
        .agent_card_url("http://localhost:9090")
        .build()?;

    server.serve().await?;
    Ok(())
}
```

| Builder Method | Default | Description |
|----------------|---------|-------------|
| `.agent(agent)` | *required* | The agent to expose |
| `.bind_addr(addr)` | `127.0.0.1:8080` | Server bind address; `0.0.0.0:8080` listens on every interface |
| `.security_config(config)` | `SecurityConfig::default()` | Hosts and origins the server answers; the agent card URL's host is always answered |
| `.session_service(svc)` | In-memory | Session backend |
| `.agent_card_url(url)` | `http://localhost:{port}` | Public base URL; the cards name `{url}/a2a/v1`, `{url}/a2a/v1/rest` and `{url}/a2a` as endpoints |
| `.skill_index(index)` | none | Advertise the skills of a `SkillIndex` in the card |

The cards' name, description, and skills come from the agent; their version is `1.0.0` and
they advertise streaming. `build()` returns an `A2aServerApp`: `serve()` binds `bind_addr` and serves
until the process stops (a port in use fails with `server.a2a.port_in_use`), and `into_router()`
returns the router for your own listener and shutdown handling.

---

## Testing with curl

Once your agent is running, verify it with these commands.

### Fetch the Agent Cards

The A2A v1 card:

```bash
curl http://localhost:8080/.well-known/agent-card.json | jq .
```

Response from the `a2a` template (`my-a2a-agent`), abbreviated:

```json
{
  "name": "my-a2a-agent",
  "description": "An AI assistant",
  "supportedInterfaces": [
    {"url": "http://localhost:8080/a2a/v1", "protocolBinding": "JSONRPC", "protocolVersion": "1.0"},
    {"url": "http://localhost:8080/a2a/v1/rest", "protocolBinding": "HTTP+JSON", "protocolVersion": "1.0"},
    {"url": "http://localhost:8080/a2a", "protocolBinding": "JSONRPC", "protocolVersion": "0.3"}
  ],
  "version": "1.0.0",
  "capabilities": {"streaming": true, "pushNotifications": false, "extendedAgentCard": false},
  "defaultInputModes": ["text/plain"],
  "defaultOutputModes": ["text/plain"],
  "skills": [
    {"id": "my-a2a-agent", "name": "model", "description": "An AI assistant I am a helpful assistant.", "tags": ["llm"]}
  ]
}
```

A v1 card has no top-level `url`; a client picks an entry of `supportedInterfaces`. The card is
served with an `ETag`. The A2A 0.3 card is at `agent.json`:

```bash
curl http://localhost:8080/.well-known/agent.json | jq .
```

```json
{
  "name": "my-a2a-agent",
  "description": "An AI assistant",
  "url": "http://localhost:8080/a2a",
  "version": "1.0.0",
  "protocolVersion": "0.3.0",
  "capabilities": {"streaming": true, "pushNotifications": false, "stateTransitionHistory": true},
  "skills": [
    {"id": "my-a2a-agent", "name": "model", "description": "An AI assistant I am a helpful assistant.", "tags": ["llm"]}
  ]
}
```

### Send a Message (A2A v1)

Every v1 request carries `A2A-Version: 1.0`, as a header or an `A2A-Version` query parameter:

```bash
curl -X POST http://localhost:8080/a2a/v1 \
  -H "Content-Type: application/json" \
  -H "A2A-Version: 1.0" \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "SendMessage",
    "params": {
      "message": {
        "messageId": "msg-1",
        "role": "ROLE_USER",
        "parts": [{"text": "Reply with the single word: pong"}]
      }
    }
  }'
```

Response (abbreviated):

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "task": {
      "id": "task-uuid",
      "contextId": "context-uuid",
      "status": {"state": "TASK_STATE_COMPLETED", "timestamp": "2026-11-20T02:49:16.930Z"},
      "artifacts": [{"artifactId": "artifact-uuid", "parts": [{"text": "pong"}]}],
      "history": [
        {"messageId": "msg-1", "role": "ROLE_USER", "parts": [{"text": "Reply with the single word: pong"}]}
      ],
      "metadata": {"adk_app_name": "my-a2a-agent", "adk_session_id": "context-uuid"}
    }
  }
}
```

A request without `A2A-Version` asks for 0.3, which the v1 endpoint does not serve: it answers
JSON-RPC error `-32009` (`VERSION_NOT_SUPPORTED`). The HTTP+JSON binding takes the JSON-RPC
`params` object as the request body and answers with `{"task": …}`:

```bash
curl -X POST "http://localhost:8080/a2a/v1/rest/message:send" \
  -H "Content-Type: application/json" \
  -H "A2A-Version: 1.0" \
  -d '{"message": {"messageId": "msg-2", "role": "ROLE_USER", "parts": [{"text": "Reply with the single word: ping"}]}}'
```

See [A2A Protocol](../deployment/a2a.md) for every v1 operation.

### Send a Message (A2A 0.3)

```bash
curl -X POST http://localhost:8080/a2a \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "message/send",
    "params": {
      "message": {
        "role": "user",
        "parts": [{"kind": "text", "text": "Reply with the single word: pong"}],
        "messageId": "msg-3"
      }
    },
    "id": "req-1"
  }'
```

Response (abbreviated):

```json
{
  "jsonrpc": "2.0",
  "id": "req-1",
  "result": {
    "id": "task-uuid",
    "contextId": "context-uuid",
    "status": {"state": "completed"},
    "artifacts": [
      {
        "artifactId": "artifact-uuid",
        "parts": [{"text": "pong"}],
        "metadata": {"adk_app_name": "my-a2a-agent", "adk_author": "my-a2a-agent"}
      }
    ]
  }
}
```

A task whose run ends on a tool call held for confirmation, or on a long-running call, ends in
`input-required`, and one that ends on an `adk_request_credential` call in `auth-required`. Model
thinking and provider-native server-tool parts are not sent to the peer. A peer's
`server_tool_call` or `server_tool_response` data part stays in the session as input, and never
reaches a model as a provider-native item.

### Stream a Response (A2A 0.3)

```bash
curl -X POST http://localhost:8080/a2a/stream \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "message/stream",
    "params": {
      "message": {
        "role": "user",
        "parts": [{"kind": "text", "text": "Explain Rust in 3 sentences."}],
        "messageId": "msg-2"
      }
    },
    "id": "req-2"
  }'
```

This returns Server-Sent Events: status updates (`submitted`, `working`), artifact updates with
`append` for each streamed chunk, a final status with `finalUpdate: true`, and `event: done`.
The v1 equivalent is `SendStreamingMessage` on `/a2a/v1`.

---

## Keep MCP and A2A boundaries distinct

MCP connects an agent application to tools, resources, and other published
capabilities. A2A connects independently deployed agents and carries the life
of their remote work. A bridge can translate between the protocols, but that
bridge is a separately deployed component with its own identity, authorization,
schema mapping, task-state mapping, and failure behavior.

ADK-Rust does not ship a binary named `mcp-a2a-server`. Do not place that
command in MCP configuration unless your deployment separately provides and
tests such a bridge. When both sides are agents, use the A2A client directly.

### Connecting from Another ADK-Rust Agent

Use `RemoteA2aAgent` to call your A2A agent from another ADK-Rust application:

```rust
use adk_rust::server::RemoteA2aAgent;

let remote = RemoteA2aAgent::builder("my-remote-agent")
    .description("Answers questions over A2A")
    .agent_url("http://localhost:8080")
    .build()?;
```

This creates an agent that forwards requests to your A2A server over the network. It resolves
the agent card at `agent_url` and sends messages only to an endpoint on the card's origin; see
[Card endpoints](../deployment/a2a.md#card-endpoints).

---

## Endpoints Reference

| Method | Path | Protocol | Description |
|--------|------|----------|-------------|
| GET | `/.well-known/agent-card.json` | v1 | Agent card: interfaces, capabilities, skills |
| POST | `/a2a/v1` | v1 | JSON-RPC binding (`SendMessage`, `GetTask`, `ListTasks`, `CancelTask`, ... ); needs `A2A-Version: 1.0` |
| POST, GET | `/a2a/v1/rest/...` | v1 | HTTP+JSON binding (`message:send`, `message:stream`, `tasks`, `tasks/{id}`, ...); needs `A2A-Version: 1.0` |
| GET | `/.well-known/agent.json` | 0.3 | Agent card with `url` and `protocolVersion: "0.3.0"` |
| POST | `/a2a` | 0.3 | JSON-RPC endpoint (`message/send`, `tasks/get`, `tasks/cancel`; the last two name the task as `{"id": …}`, or `{"taskId": …}`) |
| POST | `/a2a/stream` | 0.3 | Streaming JSON-RPC (`message/stream`) |

---

## Next Steps

- `a2a_quickstart` in [adk-playground](https://github.com/zavora-ai/adk-playground) — minimal working example
- [Tool Integration](../tools/function-tools.md) — add custom tools to your A2A agent
- [Sessions](../sessions/sessions.md) — persist conversation state across requests
- [Deployment](../deployment/server.md) — deploy your A2A agent to production

---

**Previous**: [Quickstart](../quickstart.md) | **Next**: [LlmAgent](../agents/llm-agent.md)
