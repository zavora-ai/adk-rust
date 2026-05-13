# ACP + Kiro CLI Example

Demonstrates using `adk-acp` to connect to Kiro CLI as an ACP agent.

## Prerequisites

- `kiro-cli` installed and logged in (`kiro-cli login`)
- `GOOGLE_API_KEY` set (for the delegate example)

## Examples

### Direct Connection

Send a prompt directly to Kiro CLI — no LLM orchestrator, just ACP:

```bash
cargo run --bin acp-kiro-direct
```

### Orchestrator Delegation

An ADK agent (Gemini) that delegates coding tasks to Kiro CLI:

```bash
export GOOGLE_API_KEY=your-key
cargo run --bin acp-kiro-delegate
```

The orchestrator decides when to use Kiro CLI based on the task. General questions are answered directly; coding tasks are delegated via ACP.

## How It Works

```
┌──────────────────────┐
│  You (terminal)      │
└──────────┬───────────┘
           │ chat
┌──────────▼───────────┐
│  ADK Orchestrator    │
│  (Gemini 2.5 Flash)  │
└──────────┬───────────┘
           │ tool call: kiro(prompt="...")
┌──────────▼───────────┐
│  AcpAgentTool        │
│  spawns kiro-cli acp │
└──────────┬───────────┘
           │ ACP protocol (stdio)
┌──────────▼───────────┐
│  Kiro CLI            │
│  (reads files, runs  │
│   commands, writes   │
│   code)              │
└──────────────────────┘
```

## Notes

- `--trust-all-tools` auto-approves all permission requests from Kiro CLI
- Each tool invocation spawns a fresh Kiro CLI process (stateless between calls)
- The working directory is passed to Kiro CLI so it operates on your project
