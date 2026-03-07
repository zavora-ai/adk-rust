# Redis Session Example

Demonstrates the `RedisSessionService` from `adk-session` with a Docker-managed Redis instance.

## Prerequisites

Docker installed and running.

## Quick Start

```bash
# Start Redis on port 6399 (avoids conflict with local Redis on 6379)
docker run -d --name adk-redis-test -p 6399:6379 redis:7-alpine

# Run the example
cargo run -p redis-session-example

# Clean up
docker stop adk-redis-test && docker rm adk-redis-test
```

## Examples

### 1. Session CRUD (default binary)

Full session lifecycle: create, get, append events, list, delete with three-tier state.

```bash
cargo run -p redis-session-example
```

### 2. LLM Chat (`redis-llm-chat`)

Interactive Gemini-powered chatbot with conversation history persisted to Redis. Supports session resume — stop and restart to pick up where you left off.

```bash
export GOOGLE_API_KEY="your-key-here"
cargo run -p redis-session-example --bin redis-llm-chat
```

## What the CRUD Example Does

1. Connects to Redis
2. Creates a session with three-tier state (app, user, session) — `temp:` keys are stripped
3. Retrieves and verifies the merged state
4. Appends an event with state deltas across all tiers
5. Verifies the updated state after the event
6. Lists sessions for the user
7. Deletes the session and verifies cleanup
