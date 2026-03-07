# PostgreSQL Examples

Self-contained examples demonstrating `PostgresSessionService` and `PostgresMemoryService` against real PostgreSQL databases.

## Prerequisites

- Docker installed and running

## Examples

### 1. Session CRUD (`postgres-session-example`)

Full session lifecycle: create, get, append events, list, delete with three-tier state.

```bash
# Start PostgreSQL (port 5499)
docker run -d --name adk-postgres-test \
  -e POSTGRES_USER=adk \
  -e POSTGRES_PASSWORD=adk_test \
  -e POSTGRES_DB=adk_sessions \
  -p 5499:5432 \
  postgres:17-alpine

cargo run -p postgres-session-example
```

### 2. Multi-Session (`postgres-multi-session-example`)

Demonstrates concurrent users, shared app-level state, event filtering (`num_recent_events`, `after`), and temp key stripping.

```bash
# Uses the same PostgreSQL container as above
cargo run -p postgres-multi-session-example
```

### 3. Memory Service (`postgres-memory-example`)

Demonstrates `PostgresMemoryService` with pgvector similarity search and keyword fallback.

```bash
# Start pgvector-enabled PostgreSQL (port 5498)
docker run -d --name adk-pgvector-test \
  -e POSTGRES_USER=adk \
  -e POSTGRES_PASSWORD=adk_test \
  -e POSTGRES_DB=adk_memory \
  -p 5498:5432 \
  pgvector/pgvector:pg17

cargo run -p postgres-memory-example
```

### 4. LLM Chat (`postgres-llm-chat`)

Interactive Gemini-powered chatbot with conversation history persisted to PostgreSQL. Supports session resume — stop and restart to pick up where you left off.

```bash
# Requires the same PostgreSQL container as above + a Gemini API key
export GOOGLE_API_KEY="your-key-here"
cargo run -p postgres-session-example --bin postgres-llm-chat
```

## Cleanup

```bash
docker stop adk-postgres-test adk-pgvector-test 2>/dev/null
docker rm adk-postgres-test adk-pgvector-test 2>/dev/null
```

## Connection Details

| Service | Port | Database | Image |
|---------|------|----------|-------|
| Sessions | 5499 | `adk_sessions` | `postgres:17-alpine` |
| Memory (pgvector) | 5498 | `adk_memory` | `pgvector/pgvector:pg17` |

Credentials: `adk` / `adk_test` for both.
