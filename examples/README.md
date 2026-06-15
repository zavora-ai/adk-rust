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
- `examples/developer_ergonomics` — developer ergonomics validation (RunnerConfigBuilder, ToolExecutionStrategy, SimpleToolContext, StatefulTool, run_str, #[tool] attributes)

## Validated Feature Examples

Standalone crates demonstrating current ADK-Rust features. Each has its own `Cargo.toml`, `README.md`, and `.env.example`.

**No API keys required:**

| Example | Feature | Run |
|---------|---------|-----|
| `examples/agent_registry` | Agent Registry REST API | `cargo run --manifest-path examples/agent_registry/Cargo.toml` |
| `examples/video_avatar` | Video Avatar configuration | `cargo run --manifest-path examples/video_avatar/Cargo.toml` |
| `examples/server_builder` | ServerBuilder + graceful shutdown | `cargo run --manifest-path examples/server_builder/Cargo.toml` |

**Dry-run mode (no external credentials):**

| Example | Feature | Run |
|---------|---------|-----|
| `examples/slack_toolset` | Slack Toolset | `cargo run --manifest-path examples/slack_toolset/Cargo.toml` |
| `examples/bigquery_toolset` | BigQuery Toolset | `cargo run --manifest-path examples/bigquery_toolset/Cargo.toml` |
| `examples/spanner_toolset` | Spanner Toolset | `cargo run --manifest-path examples/spanner_toolset/Cargo.toml` |

**Requires `GOOGLE_API_KEY`:**

| Example | Feature | Run |
|---------|---------|-----|
| `examples/yaml_agent` | YAML Agent Definition | `cargo run --manifest-path examples/yaml_agent/Cargo.toml` |
| `examples/mcp_sampling` | MCP Sampling | `cargo build --manifest-path examples/mcp_sampling/Cargo.toml && cargo run --manifest-path examples/mcp_sampling/Cargo.toml --bin sampling-client` |
| `examples/secret_provider` | Secret Provider | `cargo run --manifest-path examples/secret_provider/Cargo.toml` |
| `examples/user_personas` | User Personas Evaluation | `cargo run --manifest-path examples/user_personas/Cargo.toml` |
| `examples/prompt_optimizer` | Prompt Optimizer | `cargo run --manifest-path examples/prompt_optimizer/Cargo.toml` |
| `examples/intra_compaction` | Intra-Compaction | `cargo run --manifest-path examples/intra_compaction/Cargo.toml` |
| `examples/knowledge_graph_agent` | Knowledge-graph memory for a text agent (remember/relate/load_memory) | `cargo run --manifest-path examples/knowledge_graph_agent/Cargo.toml` |
| `examples/live_translation` | Real-time speech translation web UI (OpenAI `gpt-realtime-translate` / Gemini 3.5 Live Translate) | `cargo run --manifest-path examples/live_translation/Cargo.toml` |
| `examples/customer_service` | Multimodal customer-service voice agent — sees the camera, reads tone, runs refund/handoff tools (OpenAI or Gemini) | `cargo run --manifest-path examples/customer_service/Cargo.toml` |

## Quick Start

```bash
git clone https://github.com/zavora-ai/adk-playground.git
cd adk-playground

# Set your API key
export GOOGLE_API_KEY="your-key"

# Run any example
cargo run --example quickstart
```
