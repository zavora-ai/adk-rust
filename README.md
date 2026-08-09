# ADK-Rust

[![CI](https://github.com/zavora-ai/adk-rust/actions/workflows/ci.yml/badge.svg)](https://github.com/zavora-ai/adk-rust/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/adk-rust.svg)](https://crates.io/crates/adk-rust)
[![docs.rs](https://docs.rs/adk-rust/badge.svg)](https://docs.rs/adk-rust)
[![Wiki](https://img.shields.io/badge/docs-Wiki-blue)](https://github.com/zavora-ai/adk-rust/wiki)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
![Rust](https://img.shields.io/badge/rust-1.95%2B-orange.svg)
[![GitHub Discussions](https://img.shields.io/github/discussions/zavora-ai/adk-rust?style=flat&logo=github&color=5865F2)](https://github.com/zavora-ai/adk-rust/discussions)

A production-ready Rust framework for building AI agents. Model-agnostic, type-safe
and async, across 42 crates for agent orchestration.

> **v2.0.0 is a major release with breaking changes.** Coming from 1.x, read the
> [migration guide](docs/official_docs/migration/1.0-to-2.0.md) — six APIs changed
> shape, and the fan-in default changed behaviour without an API change. The
> [CHANGELOG](CHANGELOG.md) has the full entry.

## Start

```bash
cargo install cargo-adk
cargo adk new my-agent
cd my-agent && cp .env.example .env   # add GOOGLE_API_KEY
cargo run
```

Or add it to an existing project:

```toml
[dependencies]
adk-rust = "2.0.0"                                        # Gemini, agents, runner, sessions
# adk-rust = { version = "2.0.0", features = ["standard"] }  # + server, auth, graph, eval
```

| Tier | Includes | Use case |
|------|----------|----------|
| `minimal` (default) | Gemini provider, agents, runner, sessions | Fast starter agents |
| `standard` | minimal + OpenAI, Anthropic, tools, memory, telemetry, server, auth, graph, eval, guardrail, plugins, artifacts, skills | Production deployment |
| `enterprise` | standard + realtime, browser, RAG, payments, AWP | Full-featured production |
| `full` | enterprise + audio, code execution, sandbox | Everything |

A tier is a starting point for ADK-Rust feature sets. Add any single capability on top of one
without moving to the next tier, so `features = ["minimal", "audio"]` gives you the
minimal build plus audio. [AGENTS.md](AGENTS.md#adk-rust-umbrella) lists every feature you can add
this way.

## One agent, end to end

```rust
use adk_rust::prelude::*;
use adk_rust::Launcher;

#[tokio::main]
async fn main() -> AnyhowResult<()> {
    dotenvy::dotenv().ok();
    let model = GeminiModel::new(&std::env::var("GOOGLE_API_KEY")?, "gemini-2.5-flash")?;

    let agent = LlmAgentBuilder::new("assistant")
        .instruction("You are a helpful assistant. Be concise and accurate.")
        .model(Arc::new(model))
        .build()?;

    Launcher::new(Arc::new(agent)).run().await?;
    Ok(())
}
```

Swap the provider by swapping the client. The agent, runner and tools are unchanged:

| Provider | Client | Feature | Key |
|----------|--------|---------|-----|
| Gemini | `GeminiModel::new(key, model)` | default | `GOOGLE_API_KEY` |
| OpenAI | `OpenAIClient::new(OpenAIConfig::new(key, model))` | `openai` | `OPENAI_API_KEY` |
| OpenAI Responses | `OpenAIResponsesClient::new(OpenAIResponsesConfig::new(key, model))` | `openai` | `OPENAI_API_KEY` |
| Anthropic | `AnthropicClient::new(AnthropicConfig::new(key, model))` | `anthropic` | `ANTHROPIC_API_KEY` |
| DeepSeek | `DeepSeekClient::chat(key)` | `deepseek` | `DEEPSEEK_API_KEY` |
| Groq | `GroqClient::new(GroqConfig::llama70b(key))` | `groq` | `GROQ_API_KEY` |
| Ollama | `OllamaModel::new(OllamaConfig::new(model))` | `ollama` | none |
| Bedrock | `BedrockClient::new(BedrockConfig::new(region, model_id)).await?` | `bedrock` | AWS credential chain |
| mistral.rs | `MistralRsModel::new(config)` | `adk-mistralrs` | none, local |

Or let it choose: `adk_rust::run(instructions, input)` picks a provider from the
environment across the features you compiled.

### Models

| Provider | Model Examples | Feature Flag |
|----------|---------------|--------------|
| Gemini | `gemini-2.5-flash`, `gemini-2.5-pro`, `gemini-3-flash-preview`, `gemini-3.1-flash-lite-preview`, `gemini-3.1-pro-preview` | (default) |
| OpenAI | `gpt-5`, `gpt-5-mini`, `gpt-5-nano` | `openai` |
| OpenAI Responses API | `gpt-4.1`, `o3`, `o4-mini` | `openai` |
| Anthropic | `claude-opus-4-8`, `claude-sonnet-4-6`, `claude-haiku-4-5` | `anthropic` |
| DeepSeek | `deepseek-chat`, `deepseek-reasoner` | `deepseek` |
| Groq | `meta-llama/llama-4-scout-17b-16e-instruct`, `llama-3.3-70b-versatile` | `groq` |
| Ollama | `qwen3.6:35b-a3b`, `qwen3.5`, `llama3.2:3b` | `ollama` |
| Fireworks AI | `accounts/fireworks/models/llama-v3p1-8b-instruct` | `openai` (preset) |
| Together AI | `meta-llama/Llama-3.3-70B-Instruct-Turbo` | `openai` (preset) |
| Mistral AI | `mistral-small-latest` | `openai` (preset) |
| Perplexity | `sonar` | `openai` (preset) |
| Cerebras | `llama-3.3-70b` | `openai` (preset) |
| SambaNova | `Meta-Llama-3.3-70B-Instruct` | `openai` (preset) |
| xAI (Grok) | `grok-3-mini` | `openai` (preset) |
| Amazon Bedrock | `anthropic.claude-sonnet-4-20250514-v1:0` | `bedrock` |
| Azure AI Inference | (endpoint-specific) | `azure-ai` |
| mistral.rs | **Gemma 4**, Phi-3, Llama, Qwen 3.5, Voxtral, FLUX | `adk-mistralrs` |

Use current-generation models. `gemini-2.0-flash` and `gemini-2.0-flash-lite` shut
down on 31 March 2026.

## What you can build

Each row links to its guide and a runnable example.

| Capability | Guide | Example |
|------------|-------|---------|
| Tools with zero boilerplate — `#[tool]` derives the schema from your arg type | [tools](docs/official_docs/tools/function-tools.md) | [`examples/coding_agent`](examples/coding_agent) |
| MCP clients and servers on `rmcp 3.1` — tools, resources, prompts, elicitation, tasks | [mcp](docs/official_docs/mcp/index.md) | [`examples/mcp_protocol_revisions`](examples/mcp_protocol_revisions) |
| Workflow agents — sequential, parallel, loop | [agents](docs/official_docs/agents/workflow-agents.md) | [`examples/multi_perspective_analysis`](examples/multi_perspective_analysis) |
| Graph workflows — checkpoints, durable resume, human-in-the-loop, subgraphs | [graph-agents](docs/official_docs/agents/graph-agents.md) | [`examples/graph_subgraph_claims`](examples/graph_subgraph_claims) |
| Coding agents — read, edit and run code in a confined workspace | [coding-agent](docs/official_docs/coding-agent/index.md) | [`examples/coding_goal`](examples/coding_goal) |
| Realtime voice and video — OpenAI Realtime, Gemini Live, Vertex, LiveKit, WebRTC | [realtime](docs/official_docs/agents/realtime-agents.md) | [`examples/realtime_voice`](examples/realtime_voice) |
| Governed computer use — approval interrupts bound to a digest | [computer-use](docs/official_docs/computer-use/index.md) | — |
| RAG — chunking, embeddings, vector search, 6 backends | [rag](docs/official_docs/tools/rag.md) | — |
| Memory — semantic search, project isolation, a bi-temporal knowledge graph | [memory](docs/official_docs/tools/memory-tools.md) | [`examples/skill_memory_improvements`](examples/skill_memory_improvements) |
| Servers — REST with SSE, A2A v1.0.0, background runs, cron | [deployment](docs/official_docs/deployment/server.md) | [`examples/awp_agent`](examples/awp_agent) |
| Agentic Web Protocol — discovery, manifests, trust levels, consent | [awp](docs/official_docs/deployment/awp.md) | [`examples/awp_agent`](examples/awp_agent) |
| Agentic commerce — ACP and AP2 with durable journals | [payments](docs/official_docs/security/payments.md) | [`examples/payments`](examples/payments) |
| Editor interop — use an ACP coding agent as a tool, or expose yours | [acp](docs/official_docs/acp/index.md) | — |
| Browser automation — 46 WebDriver tools | [browser-tools](docs/official_docs/tools/browser-tools.md) | — |
| Evaluation — trajectory, rubric, LLM-judge, A/B, CI output | [evaluation](docs/official_docs/evaluation/evaluation.md) | [`examples/eval_showcase`](examples/eval_showcase) |
| Guardrails, RBAC, SSO, audit logging | [security](docs/official_docs/security/access-control.md) | — |
| Observability — OpenTelemetry tracing, structured logging | [observability](docs/official_docs/observability/telemetry.md) | — |

Scaffold any of it: `cargo adk new my-agent --template graph --addon telemetry`.
Run `cargo adk templates` and `cargo adk addons` for the full list.

## Crates

| Crate | Purpose | Key Features |
|-------|---------|--------------|
| `adk-core` | Foundational traits and types | `Agent` trait, `Content`, `Part`, error types, streaming primitives |
| `adk-agent` | Agent implementations | `LlmAgent`, `SequentialAgent`, `ParallelAgent`, `LoopAgent`, builder patterns |
| `adk-skill` | AgentSkills parsing and selection | Skill markdown parser, `.skills` discovery/indexing, lexical matching, prompt injection helpers |
| `adk-model` | LLM integrations | Gemini, OpenAI, Anthropic, DeepSeek, Groq, Ollama, Bedrock, Azure AI + OpenAI-compatible presets (Fireworks, Together, Mistral, Perplexity, Cerebras, SambaNova, xAI) |
| `adk-gemini` | Gemini client | Google Gemini API client with streaming and multimodal support |
| `adk-anthropic` | Anthropic client | Dedicated Anthropic API client with streaming, thinking, caching, citations, vision, PDF, pricing |
| `adk-mistralrs` | Native local inference | mistral.rs v0.8 — **Gemma 4**, Qwen 3.5, Voxtral, ISQ/MXFP4 quantization, LoRA adapters |
| `adk-tool` | Tool system and extensibility | Typed Rust tools, provider-native tools, composable toolsets, MCP clients and server SDK, dynamic local-server management |
| `adk-devtools` | Coding-agent dev tools | `read_file`/`write_file`/`edit_file`/`glob`/`grep`/`bash` as a `DevToolset`, scoped to a sandboxed `Workspace` |
| `adk-session` | Session and state management | SQLite/in-memory backends, conversation history, state persistence |
| `adk-artifact` | Artifact storage system | File-based storage, MIME type handling, image/PDF/video support |
| `adk-memory` | Long-term memory | Vector embeddings, semantic search, project-scoped isolation, bi-temporal knowledge graph (`GraphMemoryService`), 6 backends |
| `adk-payments` | Agentic commerce orchestration | ACP/AP2 adapters, canonical transaction kernel, durable journals, evidence-backed payment flows |
| `awp-types` | AWP protocol types | Trust levels, requester types, discovery documents, capability manifests, payment intents, typed A2A messages — zero `adk-*` deps |
| `adk-awp` | Agentic Web Protocol implementation | Business context loading, discovery/manifest generation, rate limiting, consent, events, health state machine, AWP routes |
| `adk-acp` | Agent Client Protocol integration | Official stable v1 client and server, one-shot and persistent sessions, streaming, cancellation, async permissions, client files and terminals, per-session MCP, and editor-facing ADK agents |
| `adk-rag` | RAG pipeline | Document chunking, embeddings, vector search, reranking, 6 backends |
| `adk-runner` | Agent execution runtime | Context management, event streaming, session lifecycle, callbacks |
| `adk-server` | Production API servers | REST API, A2A v1.0.0 protocol (11 JSON-RPC operations; `tasks/resubscribe` returns a snapshot, not a live re-attach), middleware, health checks |
| `adk-cli` | Command-line interface | Interactive REPL, session management, MCP server integration |
| `adk-realtime` | Real-time voice & multimodal agents | OpenAI Realtime + Gemini Live, bidirectional audio, video frames, VAD, affective dialogue, server-side tools via `IntegratedRealtimeRunner` |
| `adk-graph` | Graph-based workflows | LangGraph-style orchestration, state management, checkpointing, human-in-the-loop |
| `adk-browser` | Browser automation | 46 WebDriver tools, navigation, forms, screenshots, PDF generation |
| `adk-computer-use` | Governed desktop automation | Deterministic graph over `computer-use-mcp`: parallel observation, digest-bound approval interrupts, single-executor mutation, verification; wire contracts + tamper-evident evaluation receipts |
| `adk-eval` | Agent evaluation | Test definitions, trajectory validation, LLM-judged scoring, rubrics |
| `adk-guardrail` | Input/output validation | PII redaction, content filtering, JSON schema validation |
| `adk-auth` | Access control | Role-based permissions, declarative scope-based security, SSO/OAuth, audit logging |
| `adk-sandbox` | Sandboxed code execution | Process/WASM backends, OS-level sandbox profiles (Seatbelt on macOS, bubblewrap on Linux; Windows AppContainer not implemented) |
| `adk-telemetry` | Observability | Structured logging, OpenTelemetry tracing, span helpers |
| `adk-managed` | Managed agent runtime (Experimental) | Provider-neutral agent execution, in-process checkpointing and event replay (state does not survive process loss) |
| `adk-enterprise` | Enterprise client SDK (Experimental) | HTTP/SSE client for managed agent service, zero runtime deps |
| `adk-plugin` | Lifecycle hooks | `EnhancedPlugin` trait, tool and model interception, priority pipeline, shared `PluginContext` |
| `adk-retry-reflect` | Retry and reflect plugin | Intercepts tool failures, injects reflection prompts, exponential backoff, circuit breaker |
| `adk-action` | Action node types | 14 deterministic node types, `StandardProperties`, variable interpolation — the shared types behind `adk-graph`'s `ActionNodeExecutor` |
| `adk-code` | Code execution substrate | Process, Docker and embedded runtimes; the kernel `adk-codeact-monty` and the code tools build on |
| `adk-codeact-monty` | Python runtime for CodeAct (Experimental) | Pydantic Monty interpreter, sandboxed OS access, suspend and resume snapshots |
| `adk-audio` | Audio processing | STT and TTS providers, Deepgram streaming, desktop capture and playback, VAD, ONNX models (Whisper, Moonshine, Kokoro) |
| `adk-bench` | Benchmarking | Framework runtime performance against real LLM APIs, and cross-framework comparison with Python ADK |
| `adk-deploy` | Deployment utilities | Targets and manifests for shipping an agent |
| `adk-rust-macros` | Procedural macros | `#[tool]` with `read_only`/`concurrency_safe`/`long_running` metadata, `#[entrypoint]` and `#[task]` for the functional API |
| `cargo-adk` | Cargo subcommand | `cargo adk new`, `templates`, `addons`, `bench`, `deploy` |
| `adk-rust` | Umbrella crate | Re-exports every crate above behind tiered feature presets — the one dependency most projects need |

> **Extracted to standalone repos:** [adk-ui](https://github.com/zavora-ai/adk-ui) (dynamic UI generation), [adk-studio](https://github.com/zavora-ai/adk-studio) (visual agent builder), [adk-playground](https://github.com/zavora-ai/adk-playground) (120+ examples).

## Performance

Measured with `cargo adk bench` against `gemini-2.5-flash`, same workload and prompt
for every framework.

| Framework | Cold Start | Agent Loop Overhead (mean) | Agent Loop Overhead (P95) | Peak RSS |
|-----------|-----------|---------------------------|--------------------------|----------|
| **ADK-Rust** | **109 ms** | **568 μs** | **615 μs** | ~15 MB |
| Gemini Python SDK | 501 ms | 253 μs | 334 μs | 69.7 MB |
| LangGraph | 502 ms | 1,228 ms | 1,228 ms | 92.7 MB |

Cold start is process launch to first API call. Overhead is turn time minus the LLM
round trip. Apple M-series, macOS, June 2026. Run it yourself with
`cargo adk bench --dry-run` to see the cost estimate first.

## Develop

```bash
devenv shell            # reproducible toolchain, or ./scripts/setup-dev.sh
make build              # cargo build --workspace
make test               # cargo nextest run --workspace
make clippy             # -D warnings
```

[AGENTS.md](AGENTS.md) documents the conventions CI enforces, including the
per-platform tool matrix and the CI cost tiers.
[CONTRIBUTING.md](CONTRIBUTING.md) covers the workflow and the required checks.

## Documentation

- [Official docs](docs/official_docs/) — guides for every capability above
- [Wiki](https://github.com/zavora-ai/adk-rust/wiki) — tutorials and quickstarts
- [docs.rs](https://docs.rs/adk-rust) — API reference
- [Examples](examples/) — 104 standalone crates, plus 120+ in the
  [playground](https://github.com/zavora-ai/adk-playground)

## Companion projects

| Project | What it is |
|---------|------------|
| [adk-studio](https://github.com/zavora-ai/adk-studio) | Visual agent builder — canvas, code generation, live testing |
| [adk-ui](https://github.com/zavora-ai/adk-ui) | Dynamic UI generation — 28 components, React client, streaming |
| [adk-playground](https://github.com/zavora-ai/adk-playground) | 120+ working examples, and a hosted playground |

## Project

- [ROADMAP.md](ROADMAP.md) — the authoritative roadmap, and why both orchestration
  APIs are supported
- [CHANGELOG.md](CHANGELOG.md) — every release
- [CONTRIBUTORS.md](CONTRIBUTORS.md) — the people who built this
- [STABILITY.md](STABILITY.md) — crate stability tiers and the deprecation policy
- [Discussions](https://github.com/zavora-ai/adk-rust/discussions) — ideas and questions

Related: [Google's ADK](https://google.github.io/adk-docs/) ·
[MCP](https://modelcontextprotocol.io/) ·
[Gemini API](https://ai.google.dev/gemini-api/docs)

### Podcast

Two episodes on what shipped and where it is going, generated end to end by
ADK-Rust's own audio stack. [Episode 2 — the v1.0.0
launch](https://www.youtube.com/watch?v=tlqaE8qeHac) · assets under
[`docs/podcast/`](docs/podcast/).

## License

Apache 2.0. See [LICENSE](LICENSE).
