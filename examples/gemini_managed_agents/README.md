# Gemini Managed Agents — Practical Examples

Four focused agents that do real work, built on the official [Managed Agents Quickstart](https://ai.google.dev/gemini-api/docs/managed-agents-quickstart) documentation.

## Agents

| Agent | What it does | Key features |
|-------|-------------|--------------|
| **Code Agent** | Writes a Python script, runs it in a sandbox, saves output to a file | `antigravity()`, `Environment::remote()`, code execution |
| **Research Agent** | Reads Hacker News, summarizes top stories, saves as markdown | Built-in `google_search` + `url_context` tools |
| **Multi-Turn Dev Agent** | Creates a Rust project in turn 1, adds tests in turn 2 | `Environment::resume()`, `previous_interaction_id` |
| **Custom Saved Agent** | Creates a reusable code reviewer, invokes it, then deletes it | `create_agent()`, `list_agents()`, `delete_agent()`, AGENTS.md |
| **Deep Research** | Launches a background research task on a technical topic | `deep_research()`, `AgentConfig`, background execution |

## Prerequisites

- Rust 1.95.0+
- A `GOOGLE_API_KEY` with [Interactions API (Beta)](https://ai.google.dev/gemini-api/docs/interactions) access

## Setup

```bash
cp .env.example .env
# Add your API key to .env
```

## Running

```bash
# Run all agents sequentially
cargo run -p gemini-managed-agents

# Run a specific agent
cargo run -p gemini-managed-agents -- code
cargo run -p gemini-managed-agents -- research
cargo run -p gemini-managed-agents -- multiturn
cargo run -p gemini-managed-agents -- custom
cargo run -p gemini-managed-agents -- deepresearch
```

## How it maps to the official docs

| Official quickstart step | This example |
|--------------------------|--------------|
| "Run your first agent interaction" | `agent_code` — single call, sandbox, code execution |
| "Continue the conversation (multi-turn)" | `agent_multiturn` — two turns, same environment |
| "Stream the response" | (streaming available via `.stream()` — not shown for simplicity) |
| "Download files from the environment" | `gemini.download_environment(env_id)` available on the client |
| "Save a managed agent" | `agent_custom` — full CRUD lifecycle |
| "Invoke the managed agent" | `agent_custom` — invokes the saved agent by ID |

## Architecture

This example uses `adk-gemini` directly (not `adk-runner`/`LlmAgent`) because managed agents are a **direct-client capability** — they run server-side with their own tool loop. The Rust API mirrors the Python SDK:

```rust
// Python: client.interactions.create(agent="antigravity-preview-05-2026", ...)
// Rust:
let interaction = gemini
    .create_interaction()
    .antigravity()
    .environment(Environment::remote())
    .input_text("Write and run a Python script...")
    .send()
    .await?;
```

## Costs

Antigravity interactions are agentic workflows — a single request triggers multiple reasoning/tool loops. Typical costs per the [official pricing](https://ai.google.dev/gemini-api/docs/pricing#pricing-for-agents):

- Simple code tasks: $0.25–$1.00
- Research tasks: $0.30–$1.30
- Complex multi-step: $0.70–$3.25

Environment compute is free during the preview period.
