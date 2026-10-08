# OrcaRouter Example

Runs an `LlmAgent` against [OrcaRouter](https://www.orcarouter.ai) through the
`OpenAICompatibleConfig::orcarouter` preset in `adk-model`.

## What This Shows

| # | Scenario | Model |
|---|----------|-------|
| 1 | Chat | `ORCAROUTER_MODEL` (default `openai/gpt-5.6-terra`) |
| 2 | Tool calling with a local `#[tool]` function | `ORCAROUTER_MODEL` |
| 3 | Second vendor through the same key and client | `anthropic/claude-sonnet-5` |

OrcaRouter exposes an OpenAI-compatible API at `https://api.orcarouter.ai/v1`.
Model IDs carry a vendor prefix, so switching vendors changes only the model string.

## Prerequisites

- **Rust 1.95+** (edition 2024)
- **`ORCAROUTER_API_KEY`** environment variable set (keys start with `sk-orca-`)
- Built with the adk-model `openai` feature (already enabled in this example's manifest)

## Run

```bash
cd examples/orcarouter
cp .env.example .env   # add your ORCAROUTER_API_KEY
cargo run
```

Set `ORCAROUTER_MODEL` to run scenarios 1 and 2 against another vendor-prefixed model ID.
