# A2A Quickstart Example

A minimal A2A (Agent-to-Agent) protocol server built with ADK-Rust in under 30 lines.

## Setup

```bash
cp .env.example .env
# Edit .env and add your GOOGLE_API_KEY
```

## Run

From this directory, so `.env` is found:

```bash
cargo run
```

The server listens on **http://127.0.0.1:8003**, prints the address it bound, and advertises
that address as the agent card `url`.

## Network exposure

The server has no authentication: anyone who can reach its address can call the agent and spend
your model quota. It listens on loopback unless `BIND_HOST` names another address.

| Variable | Default | Effect |
|----------|---------|--------|
| `BIND_HOST` | `127.0.0.1` | Address to listen on. `0.0.0.0` listens on every interface and logs a warning. |

```bash
BIND_HOST=0.0.0.0 cargo run
```

Before listening beyond loopback, put authentication in front of the server (see
[Access control](../../docs/official_docs/security/access-control.md)).

## Endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/.well-known/agent-card.json` | Agent card (capabilities, skills) |
| POST | `/a2a` | JSON-RPC endpoint (message/send) |

## Test with curl

### Fetch agent card

```bash
curl http://localhost:8003/.well-known/agent-card.json | jq .
```

Expected response:
```json
{
  "name": "a2a-quickstart-agent",
  "description": "A minimal A2A-capable AI assistant built with ADK-Rust",
  "url": "http://127.0.0.1:8003/a2a",
  "version": "1.0.0",
  "capabilities": {
    "streaming": true,
    "pushNotifications": false,
    "stateTransitionHistory": true
  },
  "skills": [...]
}
```

### Send a message

```bash
curl -X POST http://localhost:8003/a2a \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "message/send",
    "params": {
      "message": {
        "role": "user",
        "parts": [{"kind": "text", "text": "What is 2 + 2?"}],
        "messageId": "test-1"
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
    "artifacts": [{"parts": [{"kind": "text", "text": "2 + 2 = 4"}]}]
  }
}
```

## Interoperability

This agent is compatible with:
- Google ADK Python agents (port 8001)
- LangGraph agents (port 8002)
- Any A2A protocol client

Use `RemoteA2aAgent` to connect TO this agent from another ADK-Rust application.
