# Managed Agents: Hello World

The simplest possible Anthropic Managed Agents session. Creates an agent, environment, and session, sends a message, streams the response, then cleans up.

## Prerequisites

- Rust 1.85.0+
- An `ANTHROPIC_API_KEY` with Managed Agents beta access

## Setup

```bash
cp ../../.env.example .env
# Add your ANTHROPIC_API_KEY to .env
```

## Running

```bash
cargo run -p managed-agents-hello
```

## What it demonstrates

1. Create a `ManagedAgentsClient` from environment
2. Create an agent with the standard agent toolset
3. Create a cloud environment
4. Create a session linking agent + environment
5. Open an SSE stream (must be opened BEFORE sending events)
6. Send a user message
7. Stream and print agent responses
8. Clean up: archive session, delete agent and environment
