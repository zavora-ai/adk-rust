# ADK-Rust Roadmap

> **Last updated:** June 2026
>
> ADK-Rust aims to become the best Rust-native platform for building, orchestrating, and deploying production AI agents across cloud, edge, enterprise, and emerging spatial environments.

## Vision

We believe AGI is intelligent orchestration: specialist capabilities working together, strong runtime foundations, secure deployment, transparent control planes, and adaptive execution across different environments.

By the end of 2026, an AI Agent built with ADK-Rust will have:

- Native agentic capabilities covering full vision, audio, software-as-tools, and feature parity with leading frameworks
- Autonomous mode and feature parity with open-source autonomous agents
- Secure internally and externally on all surfaces for enterprises
- Fully transparent control plane
- Physical/spatial capabilities
- Mature developer tools

---

## Status Legend

| Symbol | Meaning |
|--------|---------|
| ✅ | Shipped and stable |
| 🚧 | In progress or experimental |
| 🔮 | Planned / not started |

---

## Q1 2026 — Foundation & Enterprise Platform

### Super Agents ✅

Autonomous agents that do vertically integrated tasks well, demonstrating:

| Capability | Status | Implementation |
|-----------|--------|----------------|
| Multi-agent coordination | ✅ | `SequentialAgent`, `ParallelAgent`, `LoopAgent`, `LlmConditionalAgent`, graph orchestration |
| Tool use | ✅ | `FunctionTool`, `#[tool]` macro, MCP integration, Google Search, browser automation |
| Memory and persistence | ✅ | `adk-memory` (6 backends), `adk-session` (SQLite, PostgreSQL, Redis, MongoDB, Firestore, Neo4j), encrypted sessions |
| Production deployment | ✅ | `adk-server` (REST + A2A v1.0.0), `adk-deploy`, `cargo adk deploy`, background runs, cron scheduling |
| Business value | ✅ | `adk-payments` (ACP/AP2), `adk-enterprise`, `adk-managed` |

### Enterprise Platform ✅

| Capability | Status | Implementation |
|-----------|--------|----------------|
| Deployment workflows | ✅ | `cargo adk deploy`, composable templates (8 base + 9 addons + 5 enterprise patterns) |
| Observability | ✅ | `adk-telemetry` (OpenTelemetry 0.31), structured logging, GenAI semantic conventions |
| Scaling | ✅ | Tokio async runtime, concurrent agent throughput, connection pooling |
| Secure configuration | ✅ | `adk-auth` (JWT, OAuth2, OIDC, SSO, cloud secret providers), encrypted sessions (AES-256-GCM) |
| Rollback and release | ✅ | `adk-eval` regression baselines, `adk-bench` regression detection (exit code 2) |
| Enterprise integration | ✅ | A2A v1.0.0, AWP, ACP, MCP, REST APIs, YAML agent config |

---

## Q2 2026 — Experimental Frontiers 🚧

### Autonomous Agents 🚧

| Target | Status | Notes |
|--------|--------|-------|
| First fully autonomous robot powered by ADK-Rust | 🔮 | Requires hardware integration layer |
| 3D world automation (Unreal Engine) | 🔮 | `adk-spatial-os` concept defined, not implemented |
| Mega Agent (all capabilities combined) | 🚧 | Core capabilities exist; integration showcase pending |

### What's Shipped in Q2 2026

| Feature | Status | Crate |
|---------|--------|-------|
| Performance benchmarking framework | ✅ | `adk-bench` — real LLM benchmarks, 4.6× faster cold start vs Python |
| Agent Client Protocol | ✅ | `adk-acp` — connect to Claude Code, Codex, Kiro CLI as tools |
| Managed agent runtime | 🚧 | `adk-managed` — durable sessions, event streaming, provider parity |
| Enterprise client SDK | 🚧 | `adk-enterprise` — lightweight HTTP/SSE client for managed agents |
| Anthropic Managed Agents | 🚧 | `adk-anthropic` managed-agents feature |
| Gemini Interactions API | 🚧 | `adk-gemini` interactions feature — server-side history, step timeline |
| Action nodes | ✅ | `adk-action` — 14 deterministic node types for graph workflows |
| Sandbox execution | ✅ | `adk-sandbox` — process/WASM backends, OS-level profiles (Seatbelt, bubblewrap, AppContainer) |
| Audio ONNX models | ✅ | `adk-audio` — Whisper, Moonshine, Kokoro, Chatterbox, Qwen3-TTS |

---

## Q3 2026 — Self-Improvement 🔮

| Target | Status | Notes |
|--------|--------|-------|
| Self-improving agents | 🔮 | Agents that evaluate their own performance and improve prompts/tools |
| Prompt optimization | ✅ | `adk-eval` prompt optimizer exists; autonomous loop pending |
| Agent-driven testing | 🚧 | `adk-eval` auto-generated test cases |
| Reflection patterns | ✅ | `adk-retry-reflect` — retry with reflection prompts, circuit breaker |

---

## Q4 2026 — Spatial OS & Platform Consolidation 🔮

| Target | Status | Notes |
|--------|--------|-------|
| Spatial OS deployment platform | 🔮 | Self-contained secure platform for super agents |
| Spatial UI/UX interfaces | 🔮 | Holographic/3D interaction patterns |
| Device-embedded agents | 🔮 | Agents on phones, cars, TVs with capability awareness |
| Hardware interfaces | 🔮 | Robotics integration layer |
| Platform consolidation | 🔮 | Unified deployment across cloud/edge/device |

---

## Core Framework — Shipped ✅

These capabilities form the foundation and are stable:

| Crate | Purpose | Status |
|-------|---------|--------|
| `adk-core` | Agent, Tool, Llm, Session traits | ✅ Stable |
| `adk-agent` | LlmAgent, workflows (seq/parallel/loop/conditional) | ✅ Stable |
| `adk-model` | 15+ LLM providers (Gemini, OpenAI, Anthropic, DeepSeek, Groq, Ollama, Bedrock, Azure AI, OpenRouter + presets) | ✅ Stable |
| `adk-gemini` | Dedicated Gemini client, Vertex AI, ThinkingConfig, built-in tools | ✅ Stable |
| `adk-anthropic` | Dedicated Anthropic client, streaming, thinking, caching, citations | ✅ Stable |
| `adk-tool` | FunctionTool, #[tool] macro, MCP (rmcp 1.6), Google Search, Slack, BigQuery, Spanner | ✅ Stable |
| `adk-runner` | Agent execution, event streaming, cancellation, callbacks | ✅ Stable |
| `adk-server` | REST + A2A v1.0.0, ServerBuilder, background runs, cron | ✅ Stable |
| `adk-session` | SQLite, PostgreSQL, Redis, MongoDB, Firestore, Neo4j, encrypted | ✅ Stable |
| `adk-memory` | Semantic search, 6 backends, project-scoped isolation | ✅ Stable |
| `adk-graph` | LangGraph-style orchestration, checkpoints, HITL, functional API | ✅ Stable |
| `adk-realtime` | OpenAI, Gemini Live, Vertex AI, LiveKit, WebRTC, video avatars | ✅ Stable |
| `adk-eval` | Trajectory, semantic, rubric, LLM-judge, A/B comparison, CI output | ✅ Stable |
| `adk-bench` | Framework benchmarking, cross-framework comparison, regression CI | ✅ New |
| `adk-auth` | JWT, OAuth2, OIDC, SSO, RBAC, cloud secret providers | ✅ Stable |
| `adk-guardrail` | PII redaction, content filtering, validation | ✅ Stable |
| `adk-telemetry` | OpenTelemetry 0.31, GenAI semantic conventions | ✅ Stable |
| `adk-browser` | 46 WebDriver tools | ✅ Stable |
| `adk-audio` | STT/TTS, Deepgram, ONNX models, desktop audio | ✅ Stable |
| `adk-rag` | Document chunking, embeddings, vector search | ✅ Stable |
| `adk-payments` | ACP/AP2 commerce, transaction journals | ✅ Stable |
| `adk-awp` | Agentic Web Protocol, discovery, consent, health | ✅ Stable |
| `adk-acp` | Agent Client Protocol integration | ✅ Stable |
| `adk-mistralrs` | Local inference, 50+ architectures, LoRA/X-LoRA | ✅ Stable |
| `adk-plugin` | Lifecycle hooks, priority pipeline | ✅ Stable |
| `adk-skill` | Skill discovery, parsing, convention-based agents | ✅ Stable |
| `adk-sandbox` | Process/WASM execution, OS-level profiles | ✅ Stable |
| `adk-action` | 14 deterministic node types | ✅ Stable |
| `adk-cli` | Interactive REPL, cargo adk deploy | ✅ Stable |
| `adk-rust-macros` | #[tool] proc macro | ✅ Stable |
| `cargo-adk` | Project scaffolding, templates, addons | ✅ Stable |
| `awp-types` | AWP protocol types (zero adk deps) | ✅ Stable |

---

## Benchmark Results (June 2026)

Real measurements against `gemini-2.5-flash`:

| Framework | Cold Start | Loop Overhead | Peak RSS |
|-----------|-----------|---------------|----------|
| **ADK-Rust** | **109 ms** | **568 μs** | ~15 MB |
| Gemini Python SDK | 501 ms | 253 μs | 69.7 MB |
| LangGraph | 502 ms | 1,228 ms | 92.7 MB |

Run `cargo adk bench --confirm-cost` to reproduce.

---

## Contributing

We welcome contributions toward any roadmap target:

- **Code**: Pick an issue or propose a feature
- **Super Agents**: Build showcase agents that demonstrate ADK-Rust capabilities
- **Spatial OS**: Help design the spatial deployment platform
- **Enterprise**: Production deployment patterns and integrations

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

---

## Projects

| Project | Focus |
|---------|-------|
| [ADK-Rust](https://github.com/zavora-ai/adk-rust) | Core framework (36 crates) |
| [ADK-Studio](https://github.com/zavora-ai/adk-studio) | Visual agent builder |
| [ADK-UI](https://github.com/zavora-ai/adk-ui) | Dynamic UI generation |
| [ADK-Playground](https://github.com/zavora-ai/adk-playground) | 120+ working examples |
| Super-Agents | Autonomous vertical agents (planned) |
| Spatial-OS | Spatial deployment platform (Q4 2026) |
| ADK-Embed | Device-embedded agents (Q4 2026) |
| Mega-Agent | All-capabilities showcase (Q2-Q3 2026) |
