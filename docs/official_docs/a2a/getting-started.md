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
  (`server` includes A2A support)
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

    let model = GeminiModel::new(&api_key, "gemini-3.7-flash")?;

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
- Agent card at `GET /.well-known/agent-card.json` and `GET /.well-known/agent.json`, with
  `http://localhost:8080/a2a` as its endpoint
- JSON-RPC endpoints at `POST /a2a` and `POST /a2a/stream`
- The server's REST API and web UI, with the [host and origin defaults](../deployment/server.md#host-origin-and-bind-defaults)

It returns the router; bind it to `127.0.0.1:8080` so the card's URL matches the listener.

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
    let model = GeminiModel::new(&api_key, "gemini-3.7-flash")?;

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
| `.agent_card_url(url)` | `http://localhost:{port}` | Public base URL; the card names `{url}/a2a` as the endpoint |
| `.skill_index(index)` | none | Advertise the skills of a `SkillIndex` in the card |

The card's name, description, and skills come from the agent; its version is `1.0.0` and it
advertises streaming. `build()` returns an `A2aServerApp`: `serve()` binds `bind_addr` and serves
until the process stops (a port in use fails with `server.a2a.port_in_use`), and `into_router()`
returns the router for your own listener and shutdown handling.

---

## Testing with curl

Once your agent is running, verify it with these commands.

### Fetch the Agent Card

```bash
curl http://localhost:8080/.well-known/agent-card.json | jq .
```

Expected response:

```json
{
  "name": "my-agent",
  "description": "A helpful AI assistant",
  "url": "http://localhost:8080/a2a",
  "version": "1.0.0",
  "protocolVersion": "0.3.0",
  "capabilities": {
    "streaming": true,
    "pushNotifications": false,
    "stateTransitionHistory": true
  },
  "skills": [
    {
      "id": "my-agent",
      "name": "my-agent",
      "description": "A helpful AI assistant",
      "tags": ["agent"]
    }
  ]
}
```

### Send a Message (JSON-RPC)

```bash
curl -X POST http://localhost:8080/a2a \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "message/send",
    "params": {
      "message": {
        "role": "user",
        "parts": [{"kind": "text", "text": "What is the A2A protocol?"}],
        "messageId": "msg-1"
      }
    },
    "id": "req-1"
  }'
```

Expected response (abbreviated):

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
        "parts": [{"text": "The A2A protocol is..."}],
        "metadata": {"adk_app_name": "my-agent", "adk_author": "my-agent"}
      }
    ]
  }
}
```

A task whose run ends on a tool call held for confirmation, or on a long-running call, ends in
`input-required`, and one that ends on an `adk_request_credential` call in `auth-required`. Model
thinking is not sent to the peer.

### Stream a Response

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
`append` for each streamed chunk, and a final status.

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

| Method | Path | Description |
|--------|------|-------------|
| GET | `/.well-known/agent-card.json` | Agent card (capabilities, skills, metadata) |
| GET | `/.well-known/agent.json` | The same agent card at the pre-0.3 path |
| POST | `/a2a` | JSON-RPC endpoint (`message/send`, `tasks/get`, `tasks/cancel`) |
| POST | `/a2a/stream` | Streaming JSON-RPC (`message/stream`) |

---

## Next Steps

- [A2A Quickstart Example](https://github.com/zavora-ai/adk-rust/tree/main/examples/a2a_quickstart) — minimal working example
- [Tool Integration](../tools/function-tools.md) — add custom tools to your A2A agent
- [Sessions](../sessions/sessions.md) — persist conversation state across requests
- [Deployment](../deployment/server.md) — deploy your A2A agent to production

---

**Previous**: [Quickstart](../quickstart.md) | **Next**: [LlmAgent](../agents/llm-agent.md)
