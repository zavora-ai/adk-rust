# OpenRouter Example

Live integration example for the native OpenRouter provider and its discovery APIs.

## What This Shows

- **`OpenRouterClient`** — native chat and responses transports
- **Routing and discovery** — model listing and credit APIs
- **Full agent stack** — Runner → LlmAgent → OpenRouterClient

## Prerequisites

- **Rust 1.95+** (edition 2024)
- **`OPENROUTER_API_KEY`** environment variable set
- Built with the adk-model `openrouter` feature (already enabled in this example's manifest)

## Run

```bash
cargo run --manifest-path examples/openrouter/Cargo.toml
```
