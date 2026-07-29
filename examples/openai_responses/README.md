# OpenAI Responses API Example

Drives `OpenAIResponsesClient` through the full ADK stack (Runner → LlmAgent → session service).

## What This Shows

- **`OpenAIResponsesClient`** — the `/v1/responses` provider, recommended for reasoning models
- **`OpenAIResponsesConfig`** — model and endpoint configuration
- **Multi-turn sessions** — history preserved across turns

## Prerequisites

- **Rust 1.95+** (edition 2024)
- **`OPENAI_API_KEY`** environment variable set

## Run

```bash
cargo run --manifest-path examples/openai_responses/Cargo.toml
```
