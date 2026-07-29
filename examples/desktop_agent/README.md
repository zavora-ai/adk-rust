# Desktop Agent Example

Controls a macOS desktop with a Gemini-powered agent driving the
`computer-use-mcp` server over MCP.

## What This Shows

- **MCP toolset over stdio** — the agent consumes `computer-use-mcp` tools through `adk-tool`
- **Full JSON Schema pass-through** — MCP schemas using `exclusiveMinimum`, tuple `items`, and similar keywords are forwarded via Gemini's `parametersJsonSchema` field instead of being lossily normalized
- **Task-driven automation** — the desktop task is supplied as a command-line argument

## Prerequisites

- **Rust 1.95+** (edition 2024)
- **`GOOGLE_API_KEY`** (or `GEMINI_API_KEY`) environment variable set
- **`computer-use-mcp`** installed: `npm install -g @zavora-ai/computer-use-mcp`
- **macOS**, with the accessibility and screen-recording permissions the MCP server requests

```bash
cp examples/desktop_agent/.env.example examples/desktop_agent/.env
# Edit .env and add your GOOGLE_API_KEY
```

## Run

```bash
cargo run --manifest-path examples/desktop_agent/Cargo.toml
```

With a custom task:

```bash
cargo run --manifest-path examples/desktop_agent/Cargo.toml -- "Open Safari and go to google.com"
```

> **Note:** this example actuates a real desktop. For the governed orchestration
> layer — approval interrupts, single-executor mutation, and verification — see
> the [`adk-computer-use`](../../adk-computer-use) crate.
