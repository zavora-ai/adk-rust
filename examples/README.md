# ADK-Rust Examples

Examples have mostly moved to the dedicated playground repository:

**[adk-playground](https://github.com/zavora-ai/adk-playground)** — 120+ examples covering agents, tools, workflows, MCP, evaluation, RAG, voice, browser automation, and more.

Also available online at https://playground.adk-rust.com

## Local Validation Crates

A small number of live integration crates still live in this repository while their
playground versions are being finalized:

- `examples/openai_server_tools` — full OpenAI native-tool example matrix covering every exported wrapper
- `examples/anthropic_server_tools` — full Anthropic native-tool example matrix for the pinned `claudius` surface
- `examples/gemini3_builtin_tools` — full Gemini native-tool example matrix plus multi-turn mixed-tool validation
- `examples/openai_responses` — end-to-end OpenAI Responses validation
- `examples/openrouter` — end-to-end OpenRouter validation through the ADK agent stack
- `examples/bedrock_test` — Bedrock smoke testing
- `examples/payments` — agentic commerce scenario index for ACP/AP2 validation paths

## Quick Start

```bash
git clone https://github.com/zavora-ai/adk-playground.git
cd adk-playground

# Set your API key
export GOOGLE_API_KEY="your-key"

# Run any example
cargo run --example quickstart
```
