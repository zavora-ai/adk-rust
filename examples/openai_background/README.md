# OpenAI Background Mode Example

Demonstrates the OpenAI Responses API **background mode** workflow. Background mode submits a request for asynchronous processing — the API returns immediately with a response ID, which you then poll until the response reaches a terminal state (completed, failed, or cancelled).

This is useful for long-running requests where you don't want to hold open a streaming connection, or when integrating with webhook-based architectures.

## Prerequisites

- Rust 1.85.0+
- `OPENAI_API_KEY` environment variable set with a valid OpenAI API key

## Running

```bash
cargo run --manifest-path examples/openai_background/Cargo.toml
```

## What It Does

1. **Submit** — Sends a request to the Responses API with `background: true` in extensions
2. **Extract ID** — Retrieves the `response_id` from `provider_metadata` in the initial response
3. **Poll** — Calls `poll_response()` in a loop with a configurable delay (`POLL_INTERVAL_SECS`)
4. **Complete** — When the response reaches a terminal state, prints the final content or error

## Configuration

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `OPENAI_API_KEY` | Yes | — | OpenAI API key |
| `POLL_INTERVAL_SECS` | No | `2` | Seconds between poll attempts |

## Related

- [`adk-model/src/openai/background.rs`](../../adk-model/src/openai/background.rs) — Background mode polling implementation
- [`adk-model/src/openai/responses_client.rs`](../../adk-model/src/openai/responses_client.rs) — OpenAI Responses API client
- [OpenAI Responses API — Background Mode](https://platform.openai.com/docs/api-reference/responses/create#responses-create-background) — Official documentation
