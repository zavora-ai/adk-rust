# MongoDB Session Example

Demonstrates the `MongoSessionService` from `adk-session` with a Docker-managed MongoDB instance.

## Prerequisites

Docker installed and running.

## Quick Start

```bash
# Start MongoDB replica set on port 27099 (transactions require replica set)
docker run -d --name adk-mongodb-test -p 27099:27017 mongo:7 --replSet rs0

# Initialize the replica set
docker exec adk-mongodb-test mongosh --eval 'rs.initiate()'

# Run the example
cargo run -p mongodb-session-example

# Clean up
docker stop adk-mongodb-test && docker rm adk-mongodb-test
```

## Examples

### 1. Session CRUD (default binary)

Full session lifecycle: create, get, append events, list, delete with three-tier state.

```bash
cargo run -p mongodb-session-example
```

### 2. LLM Chat (`mongodb-llm-chat`)

Interactive Gemini-powered chatbot with conversation history persisted to MongoDB. Supports session resume — stop and restart to pick up where you left off.

```bash
export GOOGLE_API_KEY="your-key-here"
cargo run -p mongodb-session-example --bin mongodb-llm-chat
```

## What the CRUD Example Does

1. Connects to MongoDB and runs migrations (creates collections and indexes)
2. Creates a session with three-tier state (app, user, session) — `temp:` keys are stripped
3. Retrieves and verifies the merged state
4. Appends an event with state deltas across all tiers
5. Verifies the updated state after the event
6. Lists sessions for the user
7. Deletes the session and verifies cleanup

## Connection Details

| Setting | Value |
|---------|-------|
| Port | 27099 |
| Database | `adk_sessions` |
| Image | `mongo:7` |
