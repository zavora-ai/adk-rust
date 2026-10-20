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
- `examples/gemini_ws_outcomes` — Gemini error objects, cut streams, blocked prompts, and block-type finish reasons, the Interactions stream's terminal event, and the Responses WebSocket transport's abandoned, incomplete, closed, failed, and rejected-handshake responses, against scripted local upstreams, plus live Gemini and OpenAI WebSocket happy paths
- `examples/openai_responses` — end-to-end OpenAI Responses validation
- `examples/release_live_checks` — live and scripted checks from the 3.0 README audit: Responses background submissions, OpenAI Realtime over WebRTC, Gemini Live without an installed crypto provider, Groq presets, `BenchRunner` memory, a `GraphAgent` turn past 50 steps, and the Ollama token cap
- `examples/stream_termination` — model streams that end early, end mid tool call, or carry an error frame fail the run instead of persisting a truncated answer, for the OpenAI-compatible, Groq, Ollama, and OpenRouter clients against a scripted local upstream, plus live OpenAI, Groq, and OpenRouter happy paths
- `examples/openrouter` — end-to-end OpenRouter validation through the ADK agent stack
- `examples/provider_correctness` — live and scripted-upstream checks of the requests the OpenAI (Chat and Responses), Anthropic, Groq, DeepSeek, Azure AI, and Ollama clients send: tool messages per function response, Responses structured output and reasoning replay, generation config fields, blank Anthropic turns, tool-result images, stalled-stream timeouts, Anthropic thinking display, and DeepSeek-V3 native tool calls
- `examples/release_consistency` — live and scripted-upstream checks of the 3.0 changelog audit findings: Gemini, Ollama, and OpenRouter generation config fields, `Timeout` for stalled requests on every client, unknown finish reasons, Responses error codes, compaction stream errors, custom eval criteria, and the `allowed_hosts` setters
- `examples/bedrock_test` — Bedrock smoke testing
- `examples/usage_accounting` — token usage reported with `null` or missing counts, through `LlmAgent` and `Runner` with the OpenAI Responses, Anthropic, Gemini, Gemini Interactions, DeepSeek, Groq and Ollama clients against a scripted upstream, plus live OpenAI, Anthropic and Gemini: turns succeed, the tool runs once, `provider_usage` is the provider's own usage object, and run budgets count it
- `examples/model_followups` — through `LlmAgent` and `Runner` against a scripted upstream: a streamed Ollama answer whose final line leaves out `prompt_eval_count` still reports its usage and raw metrics and is counted by a run budget, DeepSeek and Azure AI stream error frames fail the run with the provider's message and category, and an OpenAI-compatible Chat Completions refusal reaches the session as the refusal text with `FinishReason::Safety`, streamed and unary, plus a live OpenAI Chat refusal
- `examples/payments` — agentic commerce scenario index for ACP/AP2 validation paths
- `examples/artifacts_eval_memory` — an `LlmAgent` and `Runner` saving and loading artifacts through tools on the in-memory, file, and GCS backends: four parallel saves keep four versions, `a:b` and `a__b` stay apart, a writer killed mid-save leaves earlier versions loadable, files in the earlier layout still load, and a session id of `user` is rejected instead of leaking into other sessions; an `adk-eval` run that rejects empty test files, empty cases, missing criteria, and zero concurrency and fails unevaluated cases; and memory recall that matches through punctuation and ranks the best match first, against a scripted GCS JSON API and live Gemini, OpenAI and Anthropic
- `examples/developer_ergonomics` — developer ergonomics validation (RunnerConfigBuilder, ToolExecutionStrategy, SimpleToolContext, StatefulTool, run_str, #[tool] attributes)
- `examples/approval_scoping` — approvals bound to the agent that held the call and the tool it was held for: two agents in a `SequentialAgent` sharing a tool name, a held MCP call whose name moves to another server or whose tool is redefined, and fingerprints reused across runs, over the SQLite approval store with a scripted OpenAI-compatible server and live OpenAI
- `examples/security_sessions_contracts` — an escalated payment held by a payment policy, decided in the approval store, and paid once when the next run resumes it; SSO grants refused when the audit sink fails, with a real `JwtValidator` against a local ES256 issuer; missing artifacts and `../` names failing as `NotFound` and `InvalidInput` on the in-memory and file backends; and invalid session identifiers failing as `session.invalid_identifier` on the in-memory, encrypted, SQLite, and Redis backends and `Runner::run_str`, against a scripted OpenAI-compatible server, a local Redis, and live OpenAI
- `examples/acp_client_host` — vendor-neutral ACP client with streamed updates, async permissions, and a workspace-bounded filesystem
- `examples/acp_kiro` — external coding-agent delegation, persistent sessions, and concurrent cancellation
- `examples/acp_server` — expose a tool-using ADK-Rust agent to an editor through stable ACP v1
- `examples/acp_openai_e2e` — live OpenAI end-to-end check: a coordinator agent delegates over ACP stdio to an ADK agent served by `AcpServer`, asserting environment passing, one-shot and persistent sessions, and the permission bridge
- `examples/acp_full_protocol` — no-API-key, Runner-backed ACP v1 server-direction reference with an end-to-end validating test (embedded-resource + multimodal prompts, permission bridge, `session/load` replay, usage/tool-call updates)
- `examples/tool_schemas` — whether a `#[tool]` tool enforces its argument schema (unknown keys, out-of-range values, broken patterns rejected with an error the model corrects, the corrected call run once), what Gemini receives of the schema (`$ref` descriptions and bounds), duplicate tool names failing at build, and `ScriptedLlm` request capture, against a scripted OpenAI-compatible server, a scripted Gemini server, and live OpenAI and Gemini
- `examples/tools_hardening` — `LlmAgent` and `Runner` on live Gemini and OpenAI against hostile MCP servers (a self-referencing schema, a hung handshake under `McpServerManager`, an empty result, a 1 MB description), a scripted Skill Registry receiving another project's skill name, `FunctionTool` and `#[tool]` argument checks with integral floats, Gemini declarations with booleans, numeric enums, unions and maps, `AgentTool` code-execution output, a stand-in container runtime whose code never ends, and hung and concurrent auth endpoints
- `examples/release_tool_fixes` — `RagTool` results without embedding vectors and a reranked `top_k` override, an MCP server's `isError` guidance that a live model follows, `AgentTool` failures without the delegate's credentials and with an `output_key` delegate's answer, notification and RSS action nodes under `HttpActionPolicy`, `/usr/bin/python3` under Seatbelt, the tool-progress truncation marker after a client stall, the MCP OAuth connect error code, `AcpServer` graceful shutdown, and the console `Launcher` printing a streamed tool call once, against scripted upstreams, stdio MCP and ACP child processes, and live Gemini
- `examples/tool_outcomes` — what `LlmAgent` tells the model when an after-tool stage fails, a tool errors with a credential in its message, a tool times out, or the model sends malformed tool-call JSON, against a scripted OpenAI-compatible server and live OpenAI
- `examples/session_backends` — `LlmAgent` and `Runner` turns on a Redis Cluster, standalone Redis sessions written by an earlier build, identifier validation on the in-memory, encrypted, SQLite, PostgreSQL, Redis, and Redis Cluster backends, and facts and tool results that survive repeated compaction under a history window, against local servers, a scripted OpenAI-compatible server, and live OpenAI
- `examples/session_integrity` — `Runner` and the `adk-server` REST and A2A endpoints over the in-memory, encrypted, SQLite, PostgreSQL, and Redis session backends: rewind scoped to one user and keeping the created-with state, a transient session lookup failure that leaves the session intact, duplicate and racing creates, typed not-found errors, event order within one millisecond, U+0000 strings, and newest-first list pages, against local servers, a scripted OpenAI-compatible server, and live OpenAI
- `examples/server_exposure` — what the `ServerBuilder` and `Launcher` servers expose and what the A2A clients accept: loopback-only listening, refusal of a cross-origin request and of a DNS-rebinding `Host` that would run the agent or read another user's sessions, `input-required` tasks for held and long-running calls (pre-v1 and v1), model thinking kept from peers, truncated A2A streams reported as errors with split multi-byte characters intact, and agent cards that name another host refused, against scripted A2A peers, a scripted OpenAI-compatible server, and live Gemini and OpenAI
- `examples/log_privacy` — with `RunConfig::record_payloads` off, a secret echoed by OpenAI-compatible, Gemini, and Anthropic 400s, retries, and stream errors, or planted in a tool error, a guardrail denial, a callback error, or a toolset error, reaches no log line or span field while the failure's code, category, and status are still logged, and caller error messages quote a bounded excerpt; with recording on the message text is logged; a caller keeps a run out of its trace with `NoSubscriber`; plus a live OpenAI 400 that echoes its input
- `examples/agent_tool_isolation` — what an agent behind an `AgentTool` inherits from its caller (topology, context cache, invocation toolsets), what it hands back when it ends on a tool result, and how its timeouts, held confirmations and forwarded events reach the caller, against scripted OpenAI-compatible servers and live OpenAI
- `examples/held_call_resume` — a call held for confirmation survives session compaction (overlap 0 and 1) and the durable worker, workflow agents (`SequentialAgent`, `LoopAgent`, `ParallelAgent`, `LlmConditionalAgent`) stop at it and resume at the holder without re-running finished sub-agents, same-name calls from `OllamaModel` without IDs keep their own arguments (also through an `AgentTool`), and the tool-call budget counts it once, over SQLite and in-memory sessions against a scripted upstream and live OpenAI
- `examples/agent_lifecycle` — agent lifecycle and callbacks: a tool's transfer obeys `disallow_transfer_to_parent`/`peers`, `${x}`, `\{x}` and `{{x}}` stay literal in instructions, `after_agent` runs after a transfer, a turn-ending tool result and a cancellation but not a held call, `after_run` runs and in-flight calls are answered when the client drops the stream, a plugin's `on_model_error` answers a failed model call with a fallback, retry-reflect budgets are per invocation and reset on success, a `before_agent` answer is stored under `output_key`, a `ParallelAgent` branch's escalation stops its siblings, `on_event` sees early-exit, cancellation and budget-stop events, three nested `AgentTool`s hold and resume a call on a 2 MiB worker thread, and `Runner::close` closes plugins and toolsets, against a scripted OpenAI-compatible server and live Gemini
- `examples/history_correctness` — what each agent's model receives and what the session keeps: `SequentialAgent` and `LoopAgent` sub-agents read earlier agents' turns as user-role context and end on a user turn, `history_max_events` windows and intra-invocation, session and context compaction never send a tool result without its call, compaction counts tool payloads and keeps history a summary would not shrink, `temp:` state lasts the invocation, streamed and rebuilt turns persist once and whole, empty turns fail, fenced JSON validates, and forwarded `AgentTool` events stay out of the caller's history, over SQLite against a scripted OpenAI-compatible server, `ScriptedLlm`, and live Gemini, Anthropic and OpenAI
- `examples/input_validation_persistence` — input guardrails screen a user message before the runner saves it: a redacted message stays redacted in later runs, a blocked one is never saved or sent, a durable worker re-checks an unscreened recovered turn, and a nested agent's guardrails apply first, over SQLite and in-memory sessions against a scripted OpenAI-compatible server and a live model
- `examples/guardrail_coverage` — output guardrails screen text a model writes beside a tool call, a guardrail block is a structured `InvalidInput` error with the guardrail's name and severity, and `RealtimeAgent` and `RealtimeRunner` screen tool results and redact tool errors before the model sees them, against scripted OpenAI-compatible and OpenAI Realtime servers and live OpenAI
- `examples/graph_correctness` — a merge after a conditional branch and reviewers re-entered by a revise loop run once per pass, an `LlmAgent` node rate-limited with `429` fails its node and is retried by its policy, a checkpointed graph tool called twice runs twice, a subgraph and a `run_node` child in a loop run on every pass, a retried parent is served the children that finished, a timeout policy leaves subgraphs and children working, and a join in a loop keeps its state bounded, through `GraphAgent` and `Runner` over a SQLite checkpointer against a scripted OpenAI-compatible server, plus one live Gemini node
- `examples/graph_runtime` — an agent node reads session state through its instruction template, node agents' events stream through `GraphAgent` and `Runner` and stay in the session, a failed step starts no queued node and cuts sibling retries short, a join reached from `START` runs once, retry delays stay within `max_delay`, two pauses in one step both reach the caller, a resume answer answers one pause, an agent's error keeps its category, concurrent children under one run id run once, and a retried `#[task]` pause and a cancelled `#[entrypoint]` workflow stop at once, over the in-memory, SQLite and Redis checkpointers against a scripted OpenAI-compatible server, live Gemini, and the live OpenAI endpoint
- `examples/autonomy_validation` — live OpenAI and Anthropic check of the autonomy-readiness Phase 0 fixes (agent transfer history, runner plugin denial, path guardrails, failing toolsets, shared session state, functional resume, eval judges, Anthropic web search replay and 1-hour cache pricing), asserting on history, counters, and usage
- `examples/harness_openai` and `examples/harness_anthropic` — the `adk-harness` walkthrough on OpenAI (`gpt-5.6-terra`) and Anthropic (`claude-sonnet-4-6`), scripted with no key or live with one: approvals held and remembered as a `ShellPrefix` rule, plan mode that writes nothing, an approved plan run as a checkpointed graph across a restart with a gate and a rewind, a durable goal that parks, survives the process and runs each approved call once, an `explore` member in its own context and members under the stricter mode, a role map that refuses `bash` without `harness:shell`, the spend report and the loop guard, and the OS sandbox keeping writes in the workspace; the scenarios are shared in `examples/harness_walkthrough`
- `examples/harness_redteam` — a scripted attacker against the `adk-harness` shell and governance (poisoned nested `AGENTS.md`, project configuration granting `bash`, `.mcp.json` spawn, git-hook persistence, background survivors, unsandboxed retry replay, digest drift, a lying `readOnlyHint`, control characters hiding a command, `~/.ssh`, network and `.env` canaries) across the 3.0 `CodingAgent`, the harness without a sandbox and the harness under Seatbelt or bubblewrap, plus live sandboxed `cargo test`, denied-write-and-retry and degraded-sandbox runs on OpenAI, Anthropic and Gemini

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
| `examples/acp_openai_e2e` | Live OpenAI client-to-server ACP check with self-verifying assertions | Set `OPENAI_API_KEY`, then `cargo run --manifest-path examples/acp_openai_e2e/Cargo.toml` |
| `examples/acp_full_protocol` | No-key, Runner-backed ACP v1 server-direction reference + validating test | `cargo test --manifest-path examples/acp_full_protocol/Cargo.toml` |

**Dry-run mode (no external credentials):**

| Example | Feature | Run |
|---------|---------|-----|
| `examples/mcp_manager` | Dynamic local MCP server registry with a deterministic Rust fixture | `cargo run --manifest-path examples/mcp_manager/Cargo.toml` |
| `examples/mcp_protocol_revisions` | MCP `2026-07-28` and SEP-2663 tasks, ending with an `LlmAgent`; ships its own server | `cargo run --manifest-path examples/mcp_protocol_revisions/Cargo.toml --bin revisions-agent` |
| `examples/mcp_hardening` | `LlmAgent` + `McpToolset` against servers that page forever, repeat names, collide under manager prefixes, or answer 5xx; optional live Gemini turn | `cargo run --manifest-path examples/mcp_hardening/Cargo.toml` |
| `examples/mcp_close` | `Runner::close` on `LlmAgent` + `McpToolset`: stdio servers exit, HTTP sessions end, composed toolsets close, a closed toolset does not reconnect; optional live Gemini turn | `cargo run --manifest-path examples/mcp_close/Cargo.toml` |
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

**Requires `DEEPSEEK_API_KEY`:**

Drives a real desktop through [computer-use-mcp](https://github.com/zavora-ai/computer-use-mcp),
so it needs an interactive session with Accessibility and Screen Recording permissions.

| Example | Feature | Run |
|---------|---------|-----|
| `examples/bi_analyst` | Reads an organisation's saved dashboards, drills into what is odd, and explains it with the numbers behind it. Open-source BI (Apache Superset) with five commercial backends; runs on a seeded fixture with no credentials | `cargo run --manifest-path examples/bi_analyst/Cargo.toml` |
| `examples/blender_console` | Multi-turn agent reporting a plan, task progress and live screen captures into a browser console a person can type into. Two MCP servers over two transports (HTTP + stdio) | `cargo run --manifest-path examples/blender_console/Cargo.toml` |
| `examples/blender_studio` | Models in Blender through its own MCP for anything with an API, and desktop pixels only where Blender exposes no controls. The desktop surface is narrowed by a real toolset filter, not by approval configuration | `cargo run --manifest-path examples/blender_studio/Cargo.toml -- "<task>"` |

**Requires `ORCAROUTER_API_KEY`:**

| Example | Feature | Run |
|---------|---------|-----|
| `examples/orcarouter` | OrcaRouter through the `OpenAICompatibleConfig::orcarouter` preset — chat, tool calling, and a second vendor on the same client | `cargo run --manifest-path examples/orcarouter/Cargo.toml` |

**Requires Google Cloud ADC (`gcloud auth application-default login`):**

| Example | Feature | Run |
|---------|---------|-----|
| `examples/example_store` | Vertex AI Example Store — few-shot upsert + similarity search against a pre-provisioned store | `cargo run --manifest-path examples/example_store/Cargo.toml` |
| `examples/vertex_sandbox` | Vertex AI Agent Engine sandbox — create, execute code with input files, delete | `cargo run --manifest-path examples/vertex_sandbox/Cargo.toml` |
| `examples/agent_orchestrator` | Agent Registry discovery + remote ReasoningEngine delegation | `cargo run --manifest-path examples/agent_orchestrator/Cargo.toml` |

## Quick Start

```bash
git clone https://github.com/zavora-ai/adk-playground.git
cd adk-playground

# Set your API key
export GOOGLE_API_KEY="your-key"

# Run any example
cargo run --example quickstart
```
