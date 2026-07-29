# Amazon Bedrock Smoke Test Example

Drives the Amazon Bedrock provider through the ADK stack, including prompt caching.

## What This Shows

- **`BedrockClient`** — the Converse API provider in `adk-model`
- **Prompt caching** — toggled with `BEDROCK_PROMPT_CACHING`
- **Model selection** — overridden with `BEDROCK_MODEL_ID`

## Prerequisites

- **Rust 1.95+** (edition 2024)
- **`AWS_REGION (or AWS_DEFAULT_REGION)`** environment variable set

Authentication uses the standard AWS credential chain — run `aws configure` first.

Optional: `BEDROCK_MODEL_ID`, `BEDROCK_PROMPT_CACHING`.

## Run

```bash
cargo run --manifest-path examples/bedrock_test/Cargo.toml
```
