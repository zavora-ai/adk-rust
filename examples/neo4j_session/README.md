# Neo4j Session Example

Demonstrates the `Neo4jSessionService` from `adk-session` with a Docker-managed Neo4j instance.

## Prerequisites

Docker installed and running.

## Quick Start

```bash
# Start Neo4j on port 7699 (Bolt) / 7474 (HTTP)
docker run -d --name adk-neo4j-test \
  -e NEO4J_AUTH=neo4j/adk_test_password \
  -p 7699:7687 -p 7474:7474 \
  neo4j:5

# Wait a few seconds for Neo4j to start, then run the example
cargo run -p neo4j-session-example

# Clean up
docker stop adk-neo4j-test && docker rm adk-neo4j-test
```

## Examples

### 1. Session CRUD (default binary)

Full session lifecycle: create, get, append events, list, delete with three-tier state.

```bash
cargo run -p neo4j-session-example
```

### 2. LLM Chat (`neo4j-llm-chat`)

Interactive Gemini-powered chatbot with conversation history persisted to Neo4j. Supports session resume — stop and restart to pick up where you left off.

```bash
export GOOGLE_API_KEY="your-key-here"
cargo run -p neo4j-session-example --bin neo4j-llm-chat
```

## What the CRUD Example Does

1. Connects to Neo4j and runs migrations (creates constraints and indexes)
2. Creates a session with three-tier state (app, user, session) — `temp:` keys are stripped
3. Retrieves and verifies the merged state
4. Appends an event with state deltas across all tiers
5. Verifies the updated state after the event
6. Lists sessions for the user
7. Deletes the session and verifies cleanup

## Connection Details

| Setting | Value |
|---------|-------|
| Bolt Port | 7699 |
| HTTP Port | 7474 |
| Username | `neo4j` |
| Password | `adk_test_password` |
| Image | `neo4j:5` |
