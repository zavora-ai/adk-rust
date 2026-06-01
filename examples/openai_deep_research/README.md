# OpenAI Deep Research Example

Demonstrates the OpenAI Responses API **deep research** models (`o3-deep-research`, `o4-mini-deep-research`). These models perform extended multi-step web research and automatically enable background mode without requiring explicit `background: true` in the request extensions. The example submits a research query, polls for progress with status updates, and prints the structured research output including citations.

Deep research models are designed for complex research tasks that require synthesizing information from multiple sources. They typically take 1–5 minutes to complete, depending on query complexity.

## Prerequisites

- Rust 1.85.0+
- `OPENAI_API_KEY` environment variable set with a valid OpenAI API key
- Access to deep research models (may require specific API tier)

## Running

```bash
cargo run --manifest-path examples/openai_deep_research/Cargo.toml
```

## What It Does

1. **Configure** — Sets up the Responses API client with a deep research model (configurable via `DEEP_RESEARCH_MODEL`)
2. **Submit** — Sends a research query; deep research models automatically enable background mode
3. **Poll** — Calls `poll_response()` in a loop with a configurable interval (`POLL_INTERVAL_SECS`, default 10s), printing progress status updates
4. **Receive** — When research completes, prints the structured output with citations from the model's web research

## Configuration

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `OPENAI_API_KEY` | Yes | — | OpenAI API key |
| `POLL_INTERVAL_SECS` | No | `10` | Seconds between poll attempts |
| `DEEP_RESEARCH_MODEL` | No | `o3-deep-research` | Model to use (`o3-deep-research` or `o4-mini-deep-research`) |

## Expected Execution Times

Deep research models perform multi-step web research and typically take longer than standard models:

- **Simple queries**: 1–2 minutes
- **Complex research**: 3–5 minutes
- **Highly detailed research**: Up to 10 minutes

The default polling interval of 10 seconds balances responsiveness with API rate limits.

## Related

- [`adk-model/src/openai/background.rs`](../../adk-model/src/openai/background.rs) — Background mode polling implementation
- [`adk-model/src/openai/responses_client.rs`](../../adk-model/src/openai/responses_client.rs) — OpenAI Responses API client
- [`examples/openai_background/`](../openai_background/) — Background mode example (explicit `background: true`)
- [OpenAI Deep Research Models](https://platform.openai.com/docs/models#o3-deep-research) — Official model documentation
