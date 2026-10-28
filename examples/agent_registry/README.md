# Agent Registry Example

Demonstrates the Agent Registry REST API from ADK-Rust v1.0 for registering, discovering, filtering, and managing agent cards.

## What This Shows

- Starting an in-process Axum HTTP server hosting the Agent Registry REST API
- Authenticating every request through a `RequestContextExtractor` (a demo token table
  naming `alice` and `bob`); an unknown token gets `401`
- Registering agent cards with metadata (name, version, tags, capabilities, endpoint URL);
  the caller who registers a card owns it
- Listing all registered agents, and retrieving a specific agent by name, as another user
- Filtering agents by tag via query parameters
- Refusing another user's `DELETE` with `403`, and replacing a card with the owner's `PUT`
- Deleting an agent and verifying removal with a 404 response
- Using `InMemoryAgentRegistryStore` as the storage backend

## Prerequisites

- Rust 1.95+
- No LLM provider or API keys required

## Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `RUST_LOG` | No | Logging level (defaults to `info`) |

## REST API Endpoints

Every request authenticates through the extractor given to
`registry_router(store, extractor)`; a request it refuses gets `401`.

| Method | Path | Who | Description |
|--------|------|-----|-------------|
| `POST` | `/api/agents` | Any authenticated caller | Register a new agent card, owned by the caller; `409` when the name is registered |
| `GET` | `/api/agents` | Any authenticated caller | List all agents (supports `tag` and `namePrefix` query filters) |
| `GET` | `/api/agents/{name}` | Any authenticated caller | Retrieve a specific agent by name |
| `PUT` | `/api/agents/{name}` | The card's owner | Replace the card; `403` for anyone else |
| `DELETE` | `/api/agents/{name}` | The card's owner | Remove an agent from the registry; `403` for anyone else |

## Run

```bash
cargo run --manifest-path examples/agent_registry/Cargo.toml
```

## Expected Output

```
╔══════════════════════════════════════════╗
║  Agent Registry — ADK-Rust v1.0        ║
╚══════════════════════════════════════════╝

📦 Created in-memory agent registry store

🚀 Agent Registry server listening on http://127.0.0.1:<port>

GET /api/agents with an unknown token → 401 Unauthorized (expected 401)

── Register Agents ──────────────────────────────
POST /api/agents (search-agent) → 201 Created
POST /api/agents (qa-agent) → 201 Created

── List All Agents (as Bob) ────────────────────
GET /api/agents → 200 OK
  Found 2 agent(s):
    - search-agent v1.0.0 (owner alice): An agent that searches the web for information
    - qa-agent v2.0.0 (owner alice): A question-answering agent for technical support

── Get Agent by Name ───────────────────────────
GET /api/agents/search-agent → 200 OK

── Filter Agents by Tag ────────────────────────
GET /api/agents?tag=search → 200 OK
  Found 2 agent(s) with tag 'search'

── Ownership ───────────────────────────────────
DELETE /api/agents/search-agent as Bob → 403 Forbidden (expected 403)
PUT /api/agents/search-agent as Alice → 200 OK

── Delete Agent and Verify ─────────────────────
DELETE /api/agents/search-agent → 204 No Content
GET /api/agents/search-agent → 404 Not Found (expected 404)

✅ Agent Registry example completed successfully.
```
