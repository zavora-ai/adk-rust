# OpenAI WebSocket Minimal

Demonstrates the lowest-level WebSocket transport for the OpenAI Responses API. Instead of using the full Runner/Agent abstraction, this example establishes a persistent WebSocket connection directly via `WsTransport`, sends a single prompt, and streams the response to stdout. WebSocket transport provides lower latency for agentic workflows by maintaining a persistent connection rather than opening a new HTTP request for each interaction.

## Prerequisites

- Rust 1.85.0+
- `OPENAI_API_KEY` environment variable set
- The `openai-ws` feature flag on `adk-model`

## Running

```bash
cargo run --manifest-path examples/openai_ws_minimal/Cargo.toml
```

## What It Does

1. Loads environment variables from `.env` (if present)
2. Establishes a persistent WebSocket connection to the OpenAI Responses API
3. Sends a single user prompt over the WebSocket
4. Streams the response tokens to stdout as they arrive
5. Prints a success message upon completion

## Related

- [`adk-model/src/openai/ws_transport.rs`](../../adk-model/src/openai/ws_transport.rs) — WebSocket transport implementation
- [`adk-model/src/openai/config.rs`](../../adk-model/src/openai/config.rs) — `OpenAIResponsesConfig` with transport options
- [OpenAI Responses API WebSocket docs](https://platform.openai.com/docs/guides/websocket)
