# Gemini Search Tool Coexistence (Issue #224) Example

Reproduction for GitHub issue #224: a built-in Google Search tool and a function tool used by the same agent.

## What This Shows

- **Built-in and function tool coexistence** — both declared on one `LlmAgent`
- **`ServerToolCall` / `ServerToolResponse`** — full provider-side tool tracing through the Runner

## Prerequisites

- **Rust 1.95+** (edition 2024)
- **`GOOGLE_API_KEY`** environment variable set

## Run

```bash
cargo run --manifest-path examples/gemini_search_bug/Cargo.toml
```
