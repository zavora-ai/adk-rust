# Managed Agents: Persistent Memory Across Sessions

Demonstrates creating a memory store, seeding it with content, running a session that writes to it, then running a second session that reads from it — proving memory persists across sessions.

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
cargo run -p managed-agents-memory
```

## What it demonstrates

1. Create a memory store with a description
2. Seed the store with initial memories (user preferences)
3. Create Session 1 that writes new information to the memory store
4. Create Session 2 that reads from the memory store and uses the information
5. List memories to verify persistence
6. Clean up: delete memories, archive store, delete agent and environment

## How memory stores work

Memory stores are mounted in the session sandbox at `/mnt/memory/`. The agent
reads and writes them using standard file tools. Each memory has a path
(like a filesystem) and text content (max 100 kB per memory).
