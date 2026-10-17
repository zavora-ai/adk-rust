# ADK-Rust Roadmap

> **Last updated:** October 2026
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
| Observability | ✅ | `adk-telemetry` (OpenTelemetry 0.32), structured logging, GenAI semantic conventions |
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

## Hardening Backlog 🔮

Gaps from the October 2026 review that remain open after the fixes in #848–#868. Each row names the follow-up fix and the pull request that recorded it.

### Realtime

| Gap | Fix | Origin |
|-----|-----|--------|
| A reconnect carries in-flight tool state onto the new connection | Defer a queued reconnect while tool calls are outstanding; reset the pending-response flags and drop outputs from the old connection | October 2026 review |
| Barge-in neither truncates played audio nor clears provider buffers | Track played milliseconds per item; on interruption send `conversation.item.truncate` (WebSocket) and `output_audio_buffer.clear` (WebRTC), and call `clear_buffer` (LiveKit) | October 2026 review |
| Event iteration continues, or hangs, after a session closes | A closing flag that makes `next_event` and `receive_raw` return `None`; drop the WebRTC sender on close; bound Gemini's close | October 2026 review |
| Provider options leak into the OpenAI session config, and transcription options are limited | Strip `language_code`, `thinking_level` and `resumeToken` before merging `extra`; merge `extra` recursively; add language and prompt to `TranscriptionConfig` | October 2026 review |
| No content guardrails on the model's own text or transcript events | Run output guardrails on realtime text and transcripts, as `LlmAgent` does for final text | #857 |
| A runner built without `tool_governance` has no payload switch, and error messages logged under `record_payloads` are not length-capped | Give realtime runners their own `record_payloads` and apply `trace_payload_max_bytes` | #857 |

### Guardrails

| Gap | Fix | Origin |
|-----|-----|--------|
| A blocked output always ends the run with an error | An option to emit and persist a replacement message on block | October 2026 review |
| Output guardrails do not see model thinking text | Screen thinking parts before they reach the client or the session | #857 |

### Held calls and delegation

| Gap | Fix | Origin |
|-----|-----|--------|
| `CodeActAgent`, realtime runners and graph tool nodes ignore an `AgentTool` delegate's hold | Surface the delegate's `ToolConfirmationRequest` and resume it, as `LlmAgent` does | #852 |
| A delegate that holds several calls in one turn surfaces them one at a time; an `AgentTool` that itself requires confirmation is asked again when it resumes | Surface every held call of the turn; carry the caller's approval across the resume | #852 |
| A call-ID decision passed without its fingerprint matches on the call ID alone | Bind call-ID decisions to the held request's fingerprint inside the approval check | #853 |
| When one `ParallelAgent` branch fails while another holds a call, the failed branch is not re-run on resume | Record failed branches in the workflow position and re-run them | #860 |
| A held call that is never decided keeps compaction from covering anything after it | An expiry for undecided holds, after which the call is answered as not run | #860 |

### Models

| Gap | Fix | Origin |
|-----|-----|--------|
| `OpenRouterClient` Chat Completions drops `refusal` | Surface it as Chat Completions now does: refusal text with `FinishReason::Safety` | #861 |
| OpenAI Responses refusals finish with `FinishReason::Stop` | Finish them with `FinishReason::Safety` | #861 |
| A refusal under `output_schema` goes through schema correction retries | Skip correction retries when a turn finishes with `FinishReason::Safety` | #861 |
| DeepSeek `finish_reason: "insufficient_system_resource"` maps to `Stop`, so a cut-off answer counts as complete | Map it to an incomplete-stream error | #861 |
| DeepSeek and Azure AI streams lack the #850 framing fixes | Read `data:` without a space and a last line without a line ending; map unknown finish reasons to `Other` | #861 |
| OpenRouter keeps the raw upstream body uncut in `AdkError` metadata | Bound it with the shared 512-byte excerpt helper | #859 |
| Bedrock stream outcomes are verified by code reading only | A scripted Converse-stream test for truncation, error events and blocked content | October 2026 review |

### Sandbox, computer use and durability

| Gap | Fix | Origin |
|-----|-----|--------|
| The Unix-local sandbox environment allowlist is fixed | Expose it on the client or sandbox config | October 2026 review |
| The computer-use graph has no deny path | Accept an explicit denial on resume and route it to the `blocked` result | October 2026 review |
| On Neo4j, a paused client keeps its transaction's locks indefinitely | A per-transaction timeout once the driver supports one; until then the server's `db.transaction.timeout` bounds it | #865 |
| No run-level switch keeps a run out of the caller's trace | A `RunConfig` switch; the `NoSubscriber` recipe in the telemetry docs covers it today | #859 |

### Build, CI and supply chain

| Gap | Fix | Origin |
|-----|-----|--------|
| Merge-queue builds skip the standalone examples, so lockfile drift between two pull requests can reach `main` | Run `cargo metadata --locked` over every example lockfile in the merge queue | #868 |
| `examples/tier_examples/*` lockfiles are stale, and `scripts/check-examples-compile.sh` does not reach them | Include them in the script, or remove them | #864 |
| `cargo clippy -p adk-model` with a single provider feature (`groq`, `ollama`, `deepseek`, `azure-ai`) fails on an unused helper | Gate the helper on the features that use it | #850 |
| `cargo clippy -p adk-payments --features ap2 --all-targets` fails in the ap2 test files | Fix the test-file lints | #853 |
| `zip` 0.6.6 reaches the `livekit` build through `webrtc-sys-build`, under a reviewed exception | Remove the exception once `webrtc-sys-build` moves to zip 9 | #864 |
| No fuzzing | `cargo-fuzz` targets for the stream decoders, the text tool-call parser, sandbox path resolution, Responses parsing, A2A decoding and the schema adapters before 3.0; an OSS-Fuzz submission after | October 2026 review |

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
| `adk-telemetry` | OpenTelemetry 0.32, GenAI semantic conventions | ✅ Stable |
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

## Two Orchestration APIs, Both Supported

ADK-Rust keeps the workflow agents (`SequentialAgent`, `ParallelAgent`,
`LoopAgent`) **and** `adk-graph`. Neither supersedes the other, and neither is
deprecated.

| | Workflow agents | `adk-graph` |
|---|---|---|
| Topology | Implied by the list of sub-agents | Declared as nodes and edges |
| Control flow | Fixed when written | May depend on results at run time |
| Durability | None beyond the session | Checkpointed per super-step |
| Cost to adopt | One constructor | A state schema and a compile step |

They compose in both directions: a workflow agent is an `AgentNode`, and
`GraphAgent` is an `Agent` a workflow agent can hold. Tests in
`adk-graph/tests/trio_composition_tests.rs` pin both, so neither API can become
something the other cannot carry.

This is a deliberate divergence from the other ADK implementations, where the
classic orchestrators are deprecated in favour of a workflow engine. Short linear
pipelines are common, and requiring a graph for them costs more than it returns.

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
| [ADK-Rust](https://github.com/zavora-ai/adk-rust) | Core framework (43 crates) |
| [ADK-Studio](https://github.com/zavora-ai/adk-studio) | Visual agent builder |
| [ADK-UI](https://github.com/zavora-ai/adk-ui) | Dynamic UI generation |
| [ADK-Playground](https://github.com/zavora-ai/adk-playground) | 120+ working examples |
| Super-Agents | Autonomous vertical agents (planned) |
| Spatial-OS | Spatial deployment platform (Q4 2026) |
| ADK-Embed | Device-embedded agents (Q4 2026) |
| Mega-Agent | All-capabilities showcase (Q2-Q3 2026) |
