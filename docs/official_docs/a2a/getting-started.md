# A2A Getting Started

Create and run an A2A (Agent-to-Agent) protocol agent in under 5 minutes.

## Prerequisites

- Rust 1.94.0 or later (`rustup update stable`)
- `cargo-adk` installed (`cargo install cargo-adk`)
- A Google API key ([get one here](https://aistudio.google.com/app/apikey))

## Scaffold an A2A Project

The fastest way to get started is the `a2a` template:

```bash
cargo adk new my-a2a-agent --template a2a
cd my-a2a-agent
```

This generates a complete project with:
- `Cargo.toml` — `adk-rust` with `features = ["standard"]` (includes A2A support)
- `src/main.rs` — A2A server using the builder API
- `.env.example` — API key placeholder

Add your API key:

```bash
cp .env.example .env
# Edit .env and set GOOGLE_API_KEY=your-key-here
```

Run:

```bash
cargo run
```

Your A2A agent is now serving on `http://localhost:8080`.

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

    let model = GeminiModel::new(api_key, "gemini-2.5-flash")?;

    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("my-agent")
            .description("A helpful AI assistant")
            .instruction("You are a helpful assistant exposed via A2A.")
            .model(Arc::new(model))
            .build()?,
    );

    let app = A2aServer::quick_start(agent);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    axum::serve(listener, app).await?;
    Ok(())
}
```

`quick_start` configures:
- In-memory session service
- Agent card at `GET /.well-known/agent.json`
- JSON-RPC endpoint at `POST /a2a`
- Streaming enabled

### Custom Config: Builder

Use the builder when you need control over port, metadata, or session backend:

```rust
use adk_rust::prelude::*;
use adk_rust::server::A2aServer;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = GeminiModel::new(api_key, "gemini-2.5-flash")?;

    let agent: Arc<dyn Agent> = Arc::new(
        LlmAgentBuilder::new("my-agent")
            .description("Production A2A agent")
            .instruction("You are a helpful assistant.")
            .model(Arc::new(model))
            .build()?,
    );

    let server = A2aServer::builder()
        .agent(agent)
        .bind_addr("0.0.0.0:9090")
        .agent_card_name("My Production Agent")
        .agent_card_description("Handles customer queries via A2A")
        .agent_card_version("2.0.0")
        .streaming(true)
        .push_notifications(false)
        .build()?;

    server.serve().await?;
    Ok(())
}
```

| Builder Method | Default | Description |
|----------------|---------|-------------|
| `.agent(agent)` | *required* | The agent to expose |
| `.bind_addr(addr)` | `0.0.0.0:8080` | Server bind address |
| `.session_service(svc)` | In-memory | Session backend |
| `.agent_card_name(name)` | `agent.name()` | Agent card display name |
| `.agent_card_description(desc)` | `agent.description()` | Agent card description |
| `.agent_card_version(ver)` | `"1.0.0"` | Agent card version |
| `.agent_card_url(url)` | `http://localhost:{port}` | Public URL for the agent |
| `.streaming(bool)` | `true` | Enable streaming responses |
| `.push_notifications(bool)` | `false` | Enable push notifications |

---

## Testing with curl

Once your agent is running, verify it with these commands.

### Fetch the Agent Card

```bash
curl http://localhost:8080/.well-known/agent.json | jq .
```

Expected response:

```json
{
  "name": "my-agent",
  "description": "A helpful AI assistant",
  "url": "http://localhost:8080",
  "version": "1.0.0",
  "capabilities": {
    "streaming": true,
    "pushNotifications": false,
    "stateTransitionHistory": true
  },
  "skills": []
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

Expected response:

```json
{
  "jsonrpc": "2.0",
  "id": "req-1",
  "result": {
    "id": "task-uuid",
    "status": {"state": "completed"},
    "artifacts": [
      {
        "parts": [{"kind": "text", "text": "The A2A protocol is..."}]
      }
    ]
  }
}
```

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

This returns Server-Sent Events with incremental task status updates.

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

let remote = RemoteA2aAgent::new(
    "my-remote-agent",
    "http://localhost:8080",
);
```

This creates an agent that forwards requests to your A2A server over the network.

---

## Endpoints Reference

| Method | Path | Description |
|--------|------|-------------|
| GET | `/.well-known/agent.json` | Agent card (capabilities, skills, metadata) |
| POST | `/a2a` | JSON-RPC endpoint (`message/send`, `message/get`, etc.) |
| POST | `/a2a/stream` | Streaming JSON-RPC (`message/stream`) |

---

## Next Steps

- [A2A Quickstart Example](https://github.com/zavora-ai/adk-rust/tree/main/examples/a2a_quickstart) — minimal working example
- [Tool Integration](../tools/function-tools.md) — add custom tools to your A2A agent
- [Sessions](../sessions/sessions.md) — persist conversation state across requests
- [Deployment](../deployment/) — deploy your A2A agent to production

---

**Previous**: [Quickstart](../quickstart.md) | **Next**: [LlmAgent](../agents/llm-agent.md)
