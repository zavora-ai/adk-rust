# UberLLM Example

Runs an `LlmAgent` against [UberLLM](https://uberllm.dev) through the
`OpenAICompatibleConfig::uberllm` preset in `adk-model`.

## What This Shows

| # | Scenario | Model |
|---|----------|-------|
| 1 | Chat | `UBERLLM_MODEL` (default `deepseek/deepseek-v4.1-flash`) |
| 2 | Tool calling with a local `#[tool]` function | `UBERLLM_MODEL` |
| 3 | Second vendor through the same key and client | `anthropic/claude-sonnet-5` |

UberLLM exposes an OpenAI-compatible API at `https://api.uberllm.dev/v1`. Model IDs carry a
vendor prefix, so switching vendors changes only the model string. `GET
https://api.uberllm.dev/v1/models` lists the chat model IDs without a key.

## Prerequisites

- **Rust 1.95+** (edition 2024)
- **`UBERLLM_API_KEY`** environment variable set (keys start with `ull_`)
- Built with the adk-model `openai` feature (already enabled in this example's manifest)

## Run

```bash
cd examples/uberllm
cp .env.example .env   # add your UBERLLM_API_KEY
cargo run
```

Set `UBERLLM_MODEL` to run scenarios 1 and 2 against another model ID.
