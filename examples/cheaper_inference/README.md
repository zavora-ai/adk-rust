# Cheaper Inference Example

Runs an `LlmAgent` against [Cheaper Inference](https://cheaperinference.com) through the
`OpenAICompatibleConfig::cheaper_inference` preset in `adk-model`.

## What This Shows

| # | Scenario | Model |
|---|----------|-------|
| 1 | Chat | `CHEAPER_INFERENCE_MODEL` (default `gpt-5.4-mini`) |
| 2 | Tool calling with a local `#[tool]` function | `CHEAPER_INFERENCE_MODEL` |
| 3 | Second vendor through the same key and client | `claude-sonnet-5` |

Cheaper Inference exposes an OpenAI-compatible API at `https://api.cheaperinference.com/v1`.
Model IDs are bare, so switching vendors changes only the model string.

## Prerequisites

- **Rust 1.95+** (edition 2024)
- **`CHEAPER_INFERENCE_API_KEY`** environment variable set (keys start with `ci_live_`)
- Built with the adk-model `openai` feature (already enabled in this example's manifest)

## Run

```bash
cd examples/cheaper_inference
cp .env.example .env   # add your CHEAPER_INFERENCE_API_KEY
cargo run
```

Set `CHEAPER_INFERENCE_MODEL` to run scenarios 1 and 2 against another model ID.
