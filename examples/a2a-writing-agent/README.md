# A2A v1.0.0 Writing Agent

An LLM-powered writing agent that takes a research summary and produces a polished article, served via the A2A v1.0.0 protocol. Includes a client binary that orchestrates the full research-to-writing pipeline.

## Architecture

The agent uses `LlmAgent` from `adk-agent` with a system instruction that directs the LLM to produce well-structured articles with introduction, body sections, and conclusion. Same server wiring as the Research Agent.

## Setup

Set at least one LLM provider API key:

| Variable | Provider | Priority |
|----------|----------|----------|
| `GOOGLE_API_KEY` | Gemini (gemini-2.5-flash) | First (default) |
| `OPENAI_API_KEY` | OpenAI (gpt-4o-mini) | Fallback |

```bash
cp .env.example .env
```

## Usage

### Start the Writing Agent

```bash
# Default: 127.0.0.1:3002
cargo run -p a2a-writing-agent
```

### Run the Research-to-Writing Pipeline

Start both agents in separate terminals, then run the client:

```bash
# Terminal 1
cargo run -p a2a-research-agent

# Terminal 2
cargo run -p a2a-writing-agent

# Terminal 3 — run the client
cargo run -p a2a-writing-agent --bin client
```

The client accepts CLI arguments:

```bash
cargo run -p a2a-writing-agent --bin client -- \
  --research-url http://127.0.0.1:3001 \
  --writing-url http://127.0.0.1:3002 \
  --topic "The future of quantum computing"
```

Or use environment variables:

```bash
RESEARCH_AGENT_URL=http://127.0.0.1:3001 \
WRITING_AGENT_URL=http://127.0.0.1:3002 \
cargo run -p a2a-writing-agent --bin client
```

### Expected Output

The client exercises the full A2A v1 protocol:

```
A2A v1.0.0 Client — Research-to-Writing Pipeline
  Research Agent: http://127.0.0.1:3001
  Writing Agent:  http://127.0.0.1:3002

=== Agent Discovery ===
  ✓ Research Agent card discovered (JSONRPC 1.0 ✓)
  ✓ Writing Agent card discovered (JSONRPC 1.0 ✓)

=== Research-to-Writing Pipeline ===
  ✓ Research Agent completed task
  ✓ Extracted research summary from artifact
  ✓ Writing Agent completed task
  ✓ Extracted article from artifact

=== Protocol Exercise: Task Operations ===
  ✓ GetTask returned research task
  ✓ ListTasks returned 1 task(s)
  ✓ SendStreamingMessage returned SSE event data

=== Protocol Exercise: Push Notifications ===
  ✓ CreateTaskPushNotificationConfig succeeded
  ✓ GetTaskPushNotificationConfig succeeded
  ✓ ListTaskPushNotificationConfigs returned 1 config(s)
  ✓ DeleteTaskPushNotificationConfig succeeded

=== Protocol Exercise: Extended Agent Card ===
  ✓ GetExtendedAgentCard returned valid card: "research-agent"

=== Protocol Exercise: Version Negotiation ===
  ✓ Response includes A2A-Version: 1.0 header
  ✓ Unsupported version (99.0) returned HTTP 400

=== Protocol Exercise: Error Paths ===
  ✓ GetTask(non-existent) returned TaskNotFoundError (-32001)
  ✓ CancelTask(completed) returned TaskNotCancelableError (-32002)

=== Protocol Exercise: RemoteA2aV1Agent ===
  ✓ RemoteA2aV1Agent name matches config
  ✓ RemoteA2aV1Agent description matches config
  ✓ RemoteA2aV1Agent has no sub-agents

=== Done ===
```

## Agent Card

- **Name**: writing-agent
- **Version**: 1.0.0
- **Skill**: Write Article (`id: "write"`)
- **Interface**: JSONRPC, protocolVersion 1.0
