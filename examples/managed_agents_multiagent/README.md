# Managed Agents: Multiagent Coordinator

Demonstrates creating multiple specialized agents, configuring a coordinator agent that orchestrates them, and observing thread activity in a multiagent session.

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
cargo run -p managed-agents-multiagent
```

## What it demonstrates

1. Create specialized worker agents (researcher, writer)
2. Create a coordinator agent that references the workers via `Multiagent::coordinator`
3. Create a session with the coordinator
4. Send a task that requires coordination between agents
5. Stream events and observe the coordinator delegating to workers
6. List session threads to see each agent's execution context
7. Clean up all resources

## How multiagent works

The coordinator agent has a `multiagent` configuration that lists other agents
as available "tools". When the coordinator decides to delegate, it spawns a
new thread for the worker agent. Each thread has its own event stream and
execution context, but shares the session's environment.
