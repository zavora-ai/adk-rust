# Feature Tier Examples

Demonstrates that all README examples work with the `minimal` default tier — no explicit features needed.

## Examples

| # | Example | What it tests | Features needed |
|---|---------|---------------|-----------------|
| 01 | `01-minimal-hello` | `adk_rust::run()` one-liner | default (minimal) |
| 02 | `02-minimal-launcher` | `Launcher::new(agent).run()` REPL | default (minimal) |
| 04 | `04-minimal-multi-provider` | Gemini + OpenAI + Anthropic auto-detect | default (minimal) |
| 05 | `05-minimal-memory` | Multi-turn conversation with session state | default (minimal) |

## Run

```bash
cd examples/tier_examples
cp .env.example .env   # add your API key

# Hello world (one-liner)
cargo run --bin 01-minimal-hello

# Interactive REPL
cargo run --bin 02-minimal-launcher

# Multi-provider auto-detect
cargo run --bin 04-minimal-multi-provider

# Multi-turn memory
cargo run --bin 05-minimal-memory
```

## Key Point

All examples use `adk-rust = "0.8.0"` with **no explicit features**. The `minimal` default includes everything needed: 3 LLM providers, tools, memory, sessions, telemetry, and the lightweight Launcher.
