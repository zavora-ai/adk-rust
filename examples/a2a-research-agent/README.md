# A2A v1.0.0 Research Agent

An LLM-powered research agent that takes a topic and produces a structured summary, served via the A2A v1.0.0 protocol.

## Architecture

The agent uses `LlmAgent` from `adk-agent` with a system instruction that directs the LLM to produce structured summaries with key findings, main points, and a brief conclusion. The server exposes all 11 A2A v1 operations via both JSON-RPC (`POST /jsonrpc`) and REST bindings, with version negotiation middleware on all routes.

```
┌─────────────────────────────────────────────┐
│  Axum Server (127.0.0.1:3001)               │
│                                             │
│  /.well-known/agent-card.json  (GET)        │
│  /jsonrpc                      (POST)       │
│  /message:send, /tasks/...     (REST)       │
│                                             │
│  ┌─────────────┐  ┌──────────────────────┐  │
│  │ LlmAgent    │  │ RequestHandler       │  │
│  │ (research)  │  │  ├─ V1Executor       │  │
│  │             │  │  ├─ InMemoryTaskStore │  │
│  │ Gemini or   │  │  └─ CachedAgentCard  │  │
│  │ OpenAI      │  │                      │  │
│  └─────────────┘  └──────────────────────┘  │
└─────────────────────────────────────────────┘
```

## Setup

Set at least one LLM provider API key:

| Variable | Provider | Priority |
|----------|----------|----------|
| `GOOGLE_API_KEY` | Gemini (gemini-2.5-flash) | First (default) |
| `OPENAI_API_KEY` | OpenAI (gpt-4o-mini) | Fallback |

Copy `.env.example` to `.env` and fill in your key:

```bash
cp .env.example .env
```

## Usage

```bash
# Start the research agent (default: 127.0.0.1:3001)
cargo run -p a2a-research-agent

# Custom host/port
HOST=0.0.0.0 PORT=8001 cargo run -p a2a-research-agent
```

On startup, the agent prints:
```
Research Agent listening on http://127.0.0.1:3001
Agent card: http://127.0.0.1:3001/.well-known/agent-card.json
LLM provider: Gemini (gemini-2.5-flash)
```

## Agent Card

Served at `GET /.well-known/agent-card.json` with ETag caching:

- **Name**: research-agent
- **Version**: 1.0.0
- **Skill**: Research & Summarize (`id: "research"`)
- **Interface**: JSONRPC, protocolVersion 1.0

## Running with the Writing Agent

See the [Writing Agent README](../a2a-writing-agent/README.md) for the full research-to-writing pipeline using the client binary.
