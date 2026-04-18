# Agent Registry Example

Demonstrates the Agent Registry REST API from ADK-Rust v0.7.0 for registering, discovering, filtering, and managing agent cards.

## What This Shows

- Starting an in-process Axum HTTP server hosting the Agent Registry REST API
- Registering agent cards with metadata (name, version, tags, capabilities, endpoint URL)
- Listing all registered agents
- Retrieving a specific agent by name
- Filtering agents by tag via query parameters
- Deleting an agent and verifying removal with a 404 response
- Using `InMemoryAgentRegistryStore` as the storage backend

## Prerequisites

- Rust 1.85+
- No LLM provider or API keys required

## Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `RUST_LOG` | No | Logging level (defaults to `info`) |

## REST API Endpoints

The Agent Registry exposes the following endpoints (all require an `Authorization` header):

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/api/agents` | Register a new agent card |
| `GET` | `/api/agents` | List all agents (supports `tag` and `namePrefix` query filters) |
| `GET` | `/api/agents/{name}` | Retrieve a specific agent by name |
| `DELETE` | `/api/agents/{name}` | Remove an agent from the registry |

## Run

```bash
cargo run --manifest-path examples/agent_registry/Cargo.toml
```

## Expected Output

```
╔══════════════════════════════════════════╗
║  Agent Registry — ADK-Rust v0.7.0        ║
╚══════════════════════════════════════════╝

📦 Created in-memory agent registry store

🚀 Agent Registry server listening on http://127.0.0.1:<port>

── Register Agents ──────────────────────────────
POST /api/agents (search-agent) → 201 Created
POST /api/agents (qa-agent) → 201 Created

── List All Agents ─────────────────────────────
GET /api/agents → 200 OK
  Found 2 agent(s):
    - search-agent v1.0.0: An agent that searches the web for information
    - qa-agent v2.0.0: A question-answering agent for technical support

── Get Agent by Name ───────────────────────────
GET /api/agents/search-agent → 200 OK

── Filter Agents by Tag ────────────────────────
GET /api/agents?tag=search → 200 OK
  Found 2 agent(s) with tag 'search'

── Delete Agent and Verify ─────────────────────
DELETE /api/agents/search-agent → 204 No Content
GET /api/agents/search-agent → 404 Not Found (expected 404)

✅ Agent Registry example completed successfully.
```
