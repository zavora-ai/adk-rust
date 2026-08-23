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
- `examples/acp_client_host` — vendor-neutral ACP client with streamed updates, async permissions, and a workspace-bounded filesystem
- `examples/acp_kiro` — external coding-agent delegation, persistent sessions, and concurrent cancellation
- `examples/acp_server` — expose a tool-using ADK-Rust agent to an editor through stable ACP v1
- `examples/acp_full_protocol` — no-API-key, Runner-backed ACP v1 server-direction reference with an end-to-end validating test (embedded-resource + multimodal prompts, permission bridge, `session/load` replay, usage/tool-call updates)

## Validated Feature Examples

Standalone crates demonstrating current ADK-Rust features. Each has its own `Cargo.toml`, `README.md`, and `.env.example`.

**No API keys required:**

| Example | Feature | Run |
|---------|---------|-----|
| `examples/agent_registry` | Agent Registry REST API | `cargo run --manifest-path examples/agent_registry/Cargo.toml` |
| `examples/video_avatar` | Video Avatar configuration | `cargo run --manifest-path examples/video_avatar/Cargo.toml` |
| `examples/server_builder` | ServerBuilder + graceful shutdown | `cargo run --manifest-path examples/server_builder/Cargo.toml` |

**ACP examples:**

| Example | Feature | Run |
|---------|---------|-----|
| `examples/acp_client_host` | External ACP agent with streamed UI events and client-controlled read-only files | Set `ACP_AGENT_COMMAND`, then `cargo run --manifest-path examples/acp_client_host/Cargo.toml` |
| `examples/acp_kiro` | Direct, delegated, persistent, and cancellable coding-agent sessions | `cargo run --manifest-path examples/acp_kiro/Cargo.toml --bin acp-kiro-session` |
| `examples/acp_server` | Gemini-backed ADK-Rust coding agent exposed to editors | Set `GOOGLE_API_KEY`, then `cargo run --manifest-path examples/acp_server/Cargo.toml` |
| `examples/acp_full_protocol` | No-key, Runner-backed ACP v1 server-direction reference + validating test | `cargo test --manifest-path examples/acp_full_protocol/Cargo.toml` |

**Dry-run mode (no external credentials):**

| Example | Feature | Run |
|---------|---------|-----|
| `examples/mcp_manager` | Dynamic local MCP server registry with a deterministic Rust fixture | `cargo run --manifest-path examples/mcp_manager/Cargo.toml` |
| `examples/mcp_protocol_revisions` | MCP `2026-07-28` and SEP-2663 tasks, ending with an `LlmAgent`; ships its own server | `cargo run --manifest-path examples/mcp_protocol_revisions/Cargo.toml --bin revisions-agent` |
| `examples/slack_toolset` | Slack Toolset | `cargo run --manifest-path examples/slack_toolset/Cargo.toml` |
| `examples/bigquery_toolset` | BigQuery Toolset | `cargo run --manifest-path examples/bigquery_toolset/Cargo.toml` |
| `examples/spanner_toolset` | Spanner Toolset | `cargo run --manifest-path examples/spanner_toolset/Cargo.toml` |

**Requires `GOOGLE_API_KEY`:**

| Example | Feature | Run |
|---------|---------|-----|
| `examples/yaml_agent` | YAML Agent Definition | `cargo run --manifest-path examples/yaml_agent/Cargo.toml` |
| `examples/mcp_sampling` | Deprecated MCP sampling compatibility | `cargo build --manifest-path examples/mcp_sampling/Cargo.toml && cargo run --manifest-path examples/mcp_sampling/Cargo.toml --bin sampling-client` |
| `examples/secret_provider` | Secret Provider | `cargo run --manifest-path examples/secret_provider/Cargo.toml` |
| `examples/user_personas` | User Personas Evaluation | `cargo run --manifest-path examples/user_personas/Cargo.toml` |
| `examples/prompt_optimizer` | Prompt Optimizer | `cargo run --manifest-path examples/prompt_optimizer/Cargo.toml` |
| `examples/intra_compaction` | Intra-Compaction | `cargo run --manifest-path examples/intra_compaction/Cargo.toml` |
| `examples/knowledge_graph_agent` | Knowledge-graph memory for a text agent (remember/relate/load_memory) | `cargo run --manifest-path examples/knowledge_graph_agent/Cargo.toml` |
| `examples/live_translation` | Real-time speech translation web UI (OpenAI `gpt-realtime-translate` / Gemini 3.5 Live Translate) | `cargo run --manifest-path examples/live_translation/Cargo.toml` |
| `examples/customer_service` | Multimodal customer-service voice agent — sees the camera, reads tone, runs refund/handoff tools (OpenAI or Gemini) | `cargo run --manifest-path examples/customer_service/Cargo.toml` |

**Requires `OPENAI_API_KEY`:**

| Example | Feature | Run |
|---------|---------|-----|
| `examples/advanced_agents` | One embedded Runtime gallery for OpenAI chat, AmbientAgent scheduling, Realtime voice, A2A, MCP `2026-07-28` tasks, and telemetry | `cargo build --manifest-path examples/advanced_agents/Cargo.toml --bins && cargo run --manifest-path examples/advanced_agents/Cargo.toml --bin advanced-runtime` |
| `examples/runtime_ui_showcase` | Embedded runtime UI gallery for a tool-calling agent, graph workflow, and portable team, with screenshots and walkthroughs | `cargo run --manifest-path examples/runtime_ui_showcase/Cargo.toml --bin runtime-ui-tools` |
| `examples/team_architectures` | Portable `TeamSpec` examples for handoff, delegation, parallel shared-state swarms, and hybrid workflows | `cargo run --manifest-path examples/team_architectures/Cargo.toml --bin team-supervisor-handoff` |

**Requires Google Cloud ADC (`gcloud auth application-default login`):**

| Example | Feature | Run |
|---------|---------|-----|
| `examples/example_store` | Vertex AI Example Store — few-shot upsert + similarity search against a pre-provisioned store | `cargo run --manifest-path examples/example_store/Cargo.toml` |
| `examples/vertex_sandbox` | Vertex AI Agent Engine sandbox — create, execute code with input files, delete | `cargo run --manifest-path examples/vertex_sandbox/Cargo.toml` |

## Quick Start

```bash
git clone https://github.com/zavora-ai/adk-playground.git
cd adk-playground

# Set your API key
export GOOGLE_API_KEY="your-key"

# Run any example
cargo run --example quickstart
```
