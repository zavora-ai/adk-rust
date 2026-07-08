# ADK-Rust

Rust Agent Development Kit — a modular workspace of publishable crates for building AI agents with tool calling, multi-model support, real-time voice, graph workflows, and more.

## Dev environment

- Rust 1.94.0+, edition 2024. Use `make setup` or `devenv shell` to bootstrap.
- `sccache` is the compilation cache. Set `RUSTC_WRAPPER=sccache` in your shell profile.
- On Linux, `wild` is the linker (configured in `.cargo/config.toml`). macOS uses the default linker.
- Copy `.env.example` to `.env` for API keys. Never commit `.env` files or secrets.
- `adk-mistralrs` is a workspace member using workspace version inheritance. GPU features (`cuda`, `metal`) are opt-in.
- `CMAKE_POLICY_VERSION_MINIMUM=3.5` is needed for cmake 4.x compatibility (audiopus).
- **Performance**: `.cargo/config.toml` sets `incremental = false` globally for `sccache` compatibility, but `Cargo.toml` `[profile.dev]` overrides this with `incremental = true` for local dev builds. CI uses the `ci` profile where the config.toml setting applies.

## Quality gates

Run all three before every commit. CI enforces them — save yourself the round-trip:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

### CI cost tiers

CI is organized into cost tiers so per-PR feedback stays fast while full coverage
is preserved (see the `ci-pipeline-restructure` spec). No coverage is dropped —
expensive axes move to a later tier.

- **PR tier** (`ci.yml` + `semver.yml`, on pull requests) — the merge-blocking
  set: `fmt` (prerequisite gate), `clippy --workspace -D warnings`,
  `nextest --workspace` (Linux, runs at most once), `feature-coverage` for
  feature-gated modules default builds skip (e.g. `adk-agent --features codeact`),
  `docs` (single `cargo doc --workspace --no-deps`), `templates`, compile-only
  `macos`/`windows` builds, and `semver` (stable strict, beta warn-only).
- **Merge tier** (`ci-merge.yml`, on `push: main`) — cross-platform
  `nextest --workspace` on macOS/Windows, the out-of-workspace Monty build, and
  doc-example compilation. Runs post-merge; not branch-protection-required.
- **Nightly tier** (`ci-nightly.yml`, on `schedule`) — the feature-combination
  matrix, `cargo-audit`/`cargo-deny` supply-chain checks, and `#[ignore]`
  integration tests gated on available secrets. Not branch-protection-required.

Only the PR tier gates merges. See CONTRIBUTING.md ("Branch Protection — Required
Status Checks") for the authoritative required-check set.

### Local git hooks (lefthook)

- **pre-commit** — `cargo fmt --all -- --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, and `shellcheck` on staged shell scripts.
- **pre-push** — `cargo check --workspace` (a fast compilation check, not the full
  test suite). CI is the full-suite safety net, so the local gate stays quick.

### AI Agent Workflow (`devenv`)

Use these shorthand scripts instead of raw `cargo` to ensure `sccache` wrap and workspace coverage:

| Action | Command | Description |
| :--- | :--- | :--- |
| **Check** | `devenv shell check` | Fast workspace compilation check. |
| **Test** | `devenv shell test` | Run all non-ignored tests (nextest). |
| **Lint** | `devenv shell clippy` | Clippy with `-D warnings` (zero tolerance). |
| **Format** | `devenv shell fmt` | Enforce Rust Edition 2024 style. |

Run `cargo fmt --all` automatically after finishing Rust code changes; do not ask for approval.

Clippy runs with `-D warnings` — every warning is a compile error. Fix warnings before pushing. Don't suppress with `#[allow(...)]` unless there's a documented reason.

## Workspace structure

Crate names are prefixed with `adk-`. The Rust module name uses underscores (`adk_core`, `adk_agent`, etc.).

### Core crates (publishable to crates.io)

```
adk-core/        Core traits and types: Agent, Tool, Llm, Session, Event, Content, State,
                 SchemaAdapter, SchemaCache, SharedState, ToolExecutionStrategy, RequestContext
adk-agent/       Agent implementations: LlmAgent, CustomAgent, SequentialAgent, ParallelAgent,
                 LoopAgent, ConditionalAgent, LlmConditionalAgent. Parallel tool dispatch,
                 SharedState coordination, skill shims.
adk-model/       LLM provider facade: Gemini, OpenAI (Chat + Responses), Anthropic, DeepSeek (V4),
                 Groq, Ollama, Bedrock, Azure AI, OpenRouter (native). Text-based tool call
                 parser (7 model formats). Provider-aware schema normalization.
                 (feature-gated)
adk-gemini/      Dedicated Gemini client with GeminiBackend trait (Studio + Vertex AI),
                 ThinkingConfig validation, built-in tool support (Search, Code Execution,
                 Maps, File Search, Computer Use). Interactions API (Beta, `interactions`
                 feature): stateful step-timeline client with server-side history,
                 streaming step events, and lifecycle (get/delete/cancel). The runtime
                 transport lives in adk-model (`gemini-interactions`): a `GeminiModel`
                 toggle that drives the standard LlmAgent/Runner through this endpoint.
adk-anthropic/   Dedicated Anthropic API client: streaming, adaptive thinking, prompt caching,
                 citations, context management, fast mode, vision, PDF processing, pricing.
                 Supports Claude Opus 4.8, Opus 4.7, Sonnet 4.6, Haiku 4.5.
                 Managed Agents API (`managed-agents` feature): agents, environments, sessions,
                 SSE streaming, custom tools, vaults, memory stores, dreams, webhooks,
                 multiagent orchestration, file mounting, self-hosted environments.
                 Files API (`files` feature): upload, download, list, get, delete.
adk-tool/        Tool system: FunctionTool, StatefulTool, SimpleToolContext, MCP integration
                 (rmcp 1.2), McpServerManager (lifecycle, health, auto-restart), MCP Resource
                 API, MCP Elicitation, Google Search, built-in tool wrappers (Gemini/OpenAI/
                 Anthropic), Slack/BigQuery/Spanner toolsets
adk-runner/      Agent execution runtime with event streaming, RunnerConfigBuilder (typestate),
                 Runner::interrupt() API, Runner::run_str(), per-session cancellation tokens
adk-server/      HTTP server (Axum) and A2A v1.0.0 (Agent-to-Agent) protocol, ServerBuilder
                 API, ShutdownHandle, YAML agent config, agent registry, auth bridge.
                 Background Runs and Cron Scheduling (`background` feature): REST endpoints
                 for async workflow execution (POST/GET/DELETE /runs), cron job management
                 (POST/GET/PATCH/DELETE /cron), concurrency policies (skip/allow/queue).
adk-session/     Session management: SQLite, PostgreSQL, Redis, MongoDB, Firestore, Neo4j
                 backends. Encrypted sessions (AES-256-GCM) with key rotation.
adk-artifact/    Binary artifact storage for agents
adk-memory/      Semantic memory and RAG search: InMemory, SQLite, PostgreSQL, Redis, MongoDB,
                 Neo4j backends. Project-scoped memory isolation.
adk-graph/       Graph-based workflow orchestration with checkpoints, durable resume,
                 HITL interrupts, ActionNodeExecutor, WorkflowSchema interchange.
                 Functional API (`functional` feature): write workflows as async functions
                 with #[entrypoint]/#[task] macros, automatic checkpointing, typed state
                 reducers (ReducedValue, UntrackedValue, MessagesValue), state schema
                 validation, interrupt/resume, and loop iteration checkpoint keying.
adk-realtime/    Real-time bidirectional audio/video streaming (OpenAI, Gemini Live, Vertex AI Live,
                 LiveKit, WebRTC), mid-session context mutation, interruption detection,
                 video avatar providers (HeyGen, D-ID)
adk-browser/     Browser automation tools via WebDriver
adk-eval/        Evaluation framework: trajectory, semantic, rubric, LLM-judge, user personas,
                 prompt optimizer, structured judge (typed verdicts), embedding similarity,
                 cost/latency tracking, trace analysis (loop/redundancy detection), regression
                 baselines, JUnit XML CI output, human annotation workflows (JSONL), A/B agent
                 comparison (Wilcoxon), auto-generated test cases, multi-turn conversation
                 metrics. Feature-gated: embedding, ci-helpers, statistics.
adk-telemetry/   OpenTelemetry 0.31 integration for agent observability
adk-guardrail/   Input/output guardrails: validation, content filtering, PII redaction
adk-auth/        Authentication: API keys, JWT, OAuth2, OIDC, SSO, cloud secret providers
                 (AWS Secrets Manager, Azure Key Vault, GCP Secret Manager)
adk-plugin/      Plugin system for agent lifecycle hooks — EnhancedPlugin trait with tool/model
                 interception, priority-based pipeline, PluginContext shared state
adk-retry-reflect/ Retry & Reflect plugin — intercepts tool failures, injects reflection prompts,
                 exponential backoff, circuit-breaker patterns
adk-skill/       Skill discovery, parsing, and convention-based agent capabilities
adk-cli/         Command-line launcher for agents, `cargo adk deploy` for ADK Platform
adk-rust-macros/ Procedural macros (#[tool] attribute with read_only, concurrency_safe,
                 long_running metadata)
adk-code/        Code execution (experimental)
adk-sandbox/     Sandboxed execution environments — process/WASM backends, OS-level sandbox profiles
                 (Seatbelt on macOS, bubblewrap on Linux, AppContainer on Windows)
adk-audio/       Audio processing, STT/TTS providers, Deepgram streaming, desktop audio
                 (capture/playback/VAD), ONNX models (Whisper, Moonshine, Kokoro, Chatterbox)
adk-rag/         Retrieval-augmented generation pipelines
adk-action/      Action node definitions (14 node types), StandardProperties, variable
                 interpolation — shared types for adk-graph ActionNodeExecutor
adk-enterprise/  Enterprise client SDK — lightweight HTTP/SSE client for the ADK-Rust Enterprise
                 Managed Agent Service. Zero adk-* runtime dependencies. Agents, sessions,
                 streaming, vaults, memory. Self-hosted support. EXPERIMENTAL.
adk-managed/     Managed agent runtime — provider-neutral, durable, resumable agent execution
                 engine. ManagedAgentRuntime trait, DefaultManagedAgentRuntime, declarative
                 ManagedAgentDef, supervised session loop with checkpointing, custom tool
                 parking, event replay, ScriptedLlm test double, golden fixture tests.
                 Feature-gated: `managed-runtime` on umbrella crate. EXPERIMENTAL.
adk-deploy/      Deployment utilities
adk-payments/    Payment integration: ACP commerce, AP2 alpha mandates, multi-actor flows
cargo-adk/       Cargo subcommand for project scaffolding and deployment
adk-rust/        Umbrella crate re-exporting all of the above with tiered feature presets
```

### Protocol crates

```
awp-types/       Agentic Web Protocol types — TrustLevel, RequesterType, AwpVersion,
                 BusinessContext, PaymentIntent, CapabilityManifest. Zero adk-* dependencies.
adk-awp/         AWP implementation (Axum 0.8): discovery, rate limiting, consent (GDPR/KPA),
                 event subscriptions (HMAC-SHA256 webhooks), health state machine, A2A messages
adk-acp/         Agent Client Protocol — connect to external ACP agents (Claude Code, Codex,
                 Kiro CLI) as tools. AcpAgentTool, AcpToolset, AcpServer (expose ADK agents
                 as ACP-compatible), StdioTransport for IDE connections.
```

### Excluded from workspace

```
adk-mistralrs/   Local LLM inference via mistral.rs v0.8 (Gemma 4, Qwen 3.5, Voxtral, Llama 4, GPT-OSS — 50+ architectures)
                 Now a workspace member (mistralrs published to crates.io). GPU features are opt-in.
                 Has its own CI workflow (.github/workflows/mistralrs-tests.yml).

adk-studio/      Visual agent builder — extracted to standalone repo.
                 Repo: https://github.com/zavora-ai/adk-studio
                 Local dev: ../adk-studio/ with path deps back to this workspace.
                 Build: cargo check --manifest-path ../adk-studio/Cargo.toml

adk-ui/          Dynamic UI generation (forms, cards, tables, charts) — extracted to standalone repo.
                 Repo: https://github.com/zavora-ai/adk-ui
                 Local dev: ../adk-ui/ with git dep on adk-core.
                 UI protocol constants are inlined in adk-server/src/ui_protocol.rs.
```

### Examples and docs

```
examples/              60 standalone example crates (each with own Cargo.toml) covering all major
                       features. Additional 120+ examples in the adk-playground repo.
docs/official_docs/    Comprehensive documentation site content
```

## Feature flags

### adk-model

`gemini` is the default. All others are opt-in:

- `gemini` (default), `openai`, `anthropic`, `deepseek`, `ollama`, `groq`
- `gemini-vertex` — Gemini via Vertex AI (Studio + Vertex backends)
- `gemini-interactions` — Gemini Interactions API (Beta); re-exports `adk_model::gemini::interactions` AND provides the runtime transport toggle on `GeminiModel` (`use_interactions_api`) that drives the standard `LlmAgent`/`Runner`/tool loop through the Interactions endpoint (allowlist, stateful continuity, `bypass_multi_tools_limit`)
- `openrouter` — OpenRouter native chat, responses, routing, discovery, and credits APIs
- `bedrock` — Amazon Bedrock via AWS SDK Converse API
- `azure-ai` — Azure AI Inference endpoints
- `all-providers` — enables all real feature flags
- `fireworks`, `together`, `mistral`, `perplexity`, `cerebras`, `sambanova`, `xai` — backward-compat aliases for `openai`. Use `OpenAICompatibleConfig` presets instead of separate client types.

### Gemini model selection

Use current-generation models. Gemini 2.0 models are deprecated (shut down March 31, 2026).

| Use case | Model ID | Notes |
|----------|----------|-------|
| Default / general | `gemini-2.5-flash` | Default in `adk-gemini` |
| Cost-efficient / high-volume | `gemini-3.1-flash-lite-preview` | Cheapest, fastest, best for agentic routing |
| Advanced reasoning | `gemini-3.1-pro-preview` | Strongest reasoning |
| Image generation | `gemini-2.5-flash-image` | Multimodal output |
| Code + agents | `gemini-3-flash-preview` | Good balance of speed and capability |

**Avoid deprecated models:** `gemini-2.0-flash`, `gemini-2.0-flash-lite` — these are shut down.

### adk-realtime

All opt-in, no defaults:

- `openai` — OpenAI Realtime API (WebSocket)
- `gemini` — Gemini Live API (WebSocket)
- `vertex-live` — Vertex AI Live API (OAuth2 via ADC, implies `gemini`)
- `livekit` — LiveKit WebRTC bridge
- `openai-webrtc` — OpenAI WebRTC transport (implies `openai`, requires cmake)
- `heygen-avatar`, `did-avatar`, `video-avatar` — Video avatar providers
- `full` — All providers except WebRTC (no cmake needed)
- `full-webrtc` — Everything including WebRTC (requires cmake)

### adk-rust (umbrella)

Four tiered presets control which crates are compiled:

- `minimal` **(default)** — agents, models, gemini, runner, sessions. Fastest possible build for a single Gemini-powered agent.
- `standard` — minimal + openai, anthropic, tools, memory, telemetry, skills, graph, auth, server, eval, guardrail, plugin, artifacts. Production deployment with server and auth.
- `enterprise` — standard + realtime, browser, rag, payments, awp. Full-featured production.
- `full` — enterprise + audio, code, sandbox. Everything.

Domain add-ons are composable with any tier: `features = ["minimal", "audio"]`.

Production backend features (require external infrastructure, NOT included in `full`):
- `postgres-session`, `redis-session`, `mongodb-session`, `firestore-session`, `neo4j-session`
- `sqlite-memory`, `database-memory`, `redis-memory`, `mongodb-memory`, `neo4j-memory`
- `auth-bridge`
- `managed-runtime` — Managed agent runtime (adk-managed): durable sessions, event streaming, provider parity

Specialist opt-in features:
- `yaml-agent`, `agent-registry` — YAML agent config and registry REST API
- `gemini-interactions` — Gemini Interactions API (Beta): wire client surface (server-side history, step timeline) plus the runtime transport on `GeminiModel` (`use_interactions_api`) driving the standard `LlmAgent`/`Runner`
- `mcp`, `mcp-http`, `mcp-sampling` — MCP transport and sampling support
- `slack`, `bigquery`, `spanner` — Native toolsets
- `action`, `action-http`, `action-trigger`, `action-db`, `action-code`, `action-email`, `action-rss`, `action-full` — Action node executors
- `video-avatar` — HeyGen/D-ID avatar providers
- `acp` — Agent Client Protocol integration
- `openrouter` — OpenRouter native APIs
- Audio ONNX models: `whisper-onnx`, `distil-whisper`, `moonshine`, `kokoro`, `chatterbox`, `qwen3-tts`

Individual features (`agents`, `models`, `tools`, `sessions`, `server`, `graph`, `realtime`, `eval`, `browser`, `auth`, `guardrail`, `plugin`, `telemetry`, `cli`, `skills`, `artifacts`, `memory`, `code`, `sandbox`, `audio`, `awp`, `acp`, `rag`, `payments`, etc.) can be selected independently.

## Rust conventions

- Always use `thiserror` for library error types. Include actionable guidance in error messages.
- Always use `async-trait` for async trait methods.
- Always use `tracing` for logging. Never `println!` or `eprintln!` in library code.
- Always use `Arc<T>` for shared ownership across async boundaries.
- Always use `tokio::sync::RwLock` for async-safe interior mutability.
- Always use builder pattern for complex configuration.
- Prefer `aws-lc-rs` as the unified `rustls` crypto provider across the workspace.
- If tests or examples panic due to "multiple crypto providers", call `adk_core::ensure_crypto_provider()` early in the entry point to force the workspace-wide default.
- When using `format!` and you can inline variables into `{}`, always do that.
- Always collapse `if` statements per clippy `collapsible_if`.
- Always inline `format!` args per clippy `uninlined_format_args`.
- Use method references over closures when possible per clippy `redundant_closure_for_method_calls`.
- When possible, make `match` statements exhaustive and avoid wildcard arms.
- Prefer `&str` over `String` in function parameters. Use `impl Into<String>` for flexible string inputs.
- Every public item needs rustdoc with `# Example` sections where practical.
- Do not create small helper methods that are referenced only once.

### Serialization conventions

- REST API and SSE payloads use `#[serde(rename_all = "camelCase")]`.
- A2A protocol types in `adk-server/src/a2a/types.rs` use `#[serde(rename_all = "lowercase")]` for enums.
- Use `#[serde(skip_serializing_if = "Option::is_none")]` for optional fields in wire types.
- Do not mix `snake_case` and `camelCase` in the same payload struct.

### Feature-gated modules

When adding code behind a feature flag, follow the existing pattern:

```rust
// In lib.rs — gate the module
#[cfg(feature = "my-feature")]
pub mod my_module;

// In lib.rs — gate the re-export
#[cfg(feature = "my-feature")]
pub use my_module::MyType;
```

### Core trait implementation notes

When implementing `Agent`:
- `sub_agents()` is required — return `&[]` for leaf agents with no children.
- `description()` returns `&str` (not `Option<&str>`).
- `run()` returns `Result<EventStream>` where `EventStream = Pin<Box<dyn Stream<Item = Result<Event>> + Send>>`.

When implementing `Tool`:
- `execute()` takes `Arc<dyn ToolContext>` and `Value`, returns `Result<Value>`.
- `parameters_schema()` and `is_long_running()` have defaults — override only when needed.
- `is_read_only()` and `is_concurrency_safe()` default to `false` — override for parallel dispatch.

When implementing `Llm`:
- `generate_content()` takes `LlmRequest` and `bool` (stream flag), returns `Result<LlmResponseStream>`.
- `schema_adapter()` returns a `SchemaAdapter` for provider-specific schema normalization (default: `GenericSchemaAdapter`).

## Error handling

`AdkError` is a structured error envelope with component, category, code, message, retry hint, and details:

```rust
use adk_core::{AdkError, ErrorComponent, ErrorCategory};

// Structured error with full context
let err = AdkError::new(
    ErrorComponent::Model,
    ErrorCategory::RateLimited,
    "model.openai.rate_limited",
    "OpenAI API rate limit exceeded",
)
.with_provider("openai")
.with_upstream_status(429);

// Backward-compatible convenience constructors (for migration)
let err = AdkError::model("something went wrong");
let err = AdkError::session("not found");

// Category checks
err.is_retryable();    // true for RateLimited, Unavailable, Timeout
err.is_not_found();
err.is_unauthorized();

// HTTP response generation
err.http_status_code(); // 429
err.to_problem_json();  // structured JSON error body
```

For crate-local errors, use `thiserror` enums internally and implement `From<CrateLocalError> for AdkError` at the boundary:

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MyError {
    #[error("descriptive message: {0}")]
    Variant(String),

    #[error("actionable guidance: {details}. Try doing X instead.")]
    WithGuidance { details: String },
}
```

Use `adk_core::AdkError` (not `adk_core::Error`) when returning errors from agent/tool implementations.

## Tool authorization

Four mechanisms for controlling tool execution, composable in any combination:

1. **`ToolConfirmationPolicy`** — built-in HITL. Pauses execution, emits a `ToolConfirmationRequest` event, waits for `Approve`/`Deny` on the next run. Works in CLI and web server via SSE events.

```rust
let agent = LlmAgentBuilder::new("assistant")
    .model(model)
    .tool(Arc::new(delete_tool))
    .require_tool_confirmation("delete_file")  // per-tool
    // .require_tool_confirmation_for_all()     // or all tools
    .build()?;
```

The agent emits `event.actions.tool_confirmation = Some(ToolConfirmationRequest { tool_name, args, function_call_id })`. Pass the decision back via `RunConfig::tool_confirmation_decisions`.

2. **`BeforeToolCallback`** — programmatic gate. Return `Ok(Some(content))` to skip, `Ok(None)` to allow.

```rust
.before_tool_callback(Box::new(|ctx| {
    Box::pin(async move {
        if ctx.tool_name().unwrap_or("") == "admin_action" {
            return Ok(Some(Content::new("tool").with_text("denied")));
        }
        Ok(None)
    })
}))
```

3. **`adk-auth` RBAC** — role-based access control with `ProtectedTool` wrapper and audit logging.

4. **Graph interrupts** (`adk-graph`) — checkpoint-based pauses with durable state for complex approval workflows.

Evaluation order: RBAC → BeforeToolCallback → ToolConfirmationPolicy → execute → AfterToolCallback.

See `docs/official_docs/security/tool-authorization.md` for full documentation with CLI and web server examples.

## Logging

See `adk-gemini/AGENTS.md` for the full tracing conventions. The key rules:

- Lowercase messages: `info!("starting generation")` not `info!("Starting generation")`
- Dot-notation fields: `info!(status.code = 200, file.size = 1024, "request completed")`
- Errors: `error = %err`. Complex types: `field = ?value`.
- Use `#[instrument(skip_all, fields(...))]` with `Span::current().record()` for deferred values.
- Log levels: `debug!` for details, `info!` for status, `warn!` for issues, `error!` for failures.

## Testing

```bash
cargo nextest run --workspace                                # full workspace (parallel)
cargo nextest run -p adk-core                                # single crate
cargo nextest run -p adk-realtime --features full            # with features
```

- Prefer `cargo nextest run` over `cargo test` for speed (~10x faster via parallel test binary execution).
- Use `cargo test` only for doctests (nextest doesn't run them) or when you need `--doc` specifically.

- Unit tests: `#[cfg(test)]` modules in source files.
- Integration tests: `tests/*.rs` in each crate.
- Property tests: `tests/*_property_tests.rs` using `proptest` with 100+ iterations.
- Doc examples: Cargo snippets, feature names, and package/example references in `README.md` and `docs/official_docs/` are validated against `cargo metadata` by `scripts/check-doc-examples.sh`.
- When writing tests, prefer comparing equality of entire objects over fields one by one.
- Tests that require API keys or external services should be `#[ignore]`.
- Run the test for the specific crate you changed first (`cargo test -p adk-<crate>`), then run the full workspace if you changed shared crates like `adk-core`.
- Crate-specific or individual tests can be run without asking the user, but ask before running the full workspace test suite.

## Documentation

- When making a change that adds or changes an API, ensure that `docs/official_docs/` is up to date.
- Documented examples (Cargo snippets, feature names, and package/example references in `README.md` and `docs/official_docs/`) are validated in CI by `scripts/check-doc-examples.sh` against `cargo metadata`. Compile coverage comes from the workspace example crates and cargo-adk templates, so if you change a public API, keep those in sync.
- Update the crate's `README.md` if capabilities changed.
- Update `CHANGELOG.md` for user-facing changes.

## Adding new code

### New LLM provider

1. Implement `adk_core::Llm` in `adk-model/src/<provider>/`
2. Add feature flag to `adk-model/Cargo.toml`
3. Re-export from `adk-model/src/lib.rs` behind the feature
4. Implement `schema_adapter()` if the provider has specific schema requirements
5. Add example in [adk-playground](https://github.com/zavora-ai/adk-playground) or as a standalone crate in `examples/`
6. Update `adk-studio` codegen if the provider should be available in Studio (separate repo: `../adk-studio/`)

### New tool

1. Implement `adk_core::Tool` trait (use `Arc<dyn ToolContext>` for context parameter)
2. Add to `adk-tool/src/` or new crate if heavy deps
3. Consider `is_read_only()` and `is_concurrency_safe()` for parallel dispatch
4. Write unit + property tests

### New example

Examples live as standalone crates in `examples/` (each with their own `Cargo.toml` and `[workspace]` key). Additional examples go to the [adk-playground](https://github.com/zavora-ai/adk-playground) repo.

Pattern for a new example crate:
```
examples/my_example/
├── Cargo.toml          # [workspace] key, path deps to workspace crates
├── src/main.rs         # Entry point with dotenvy, tracing, banner
├── README.md           # Documentation
└── .env.example        # Required env vars
```

## Commit messages

Use conventional commits:

```
feat(gemini): add Vertex AI streaming backend
fix(realtime): correct audio delta byte handling
docs: update CONTRIBUTING.md
refactor(ui): extract validation into per-type trait impls
test(eval): add trajectory property tests
```

## PR workflow

- **Branch naming**: Use `prefix/short-description`. Allowed prefixes: `feat/`, `fix/`, `docs/`, `refactor/`, `test/`, `chore/`.
- **Reference**: Include `Fixes #123` or similar in the PR description.
- **Scope**: Keep PRs focused — one logical change per PR. Don't mix unrelated changes.
- **Quality Gates**: All four gates must pass before merge: `fmt`, `clippy`, `test`, `check` (via `devenv shell`).

### PR Checklist requirements

1. **Quality**:
   - New code has tests (unit, integration, or property tests).
   - Public APIs have rustdoc comments with `# Example` sections.
   - No `println!`/`eprintln!` in library code (use `tracing` instead).
   - No hardcoded secrets, API keys, or local paths.
2. **Hygiene**:
   - No local development artifacts (`.env`, `.DS_Store`, IDE configs, build dirs).
   - Commit messages follow conventional format.
   - Branch targets `main` branch.
3. **Documentation**:
   - `CHANGELOG.md` updated for user-facing changes.
   - `README.md` updated if crate capabilities changed.
   - Examples added or updated for new features.

## Dependency changes

- If you change `Cargo.toml` or `Cargo.lock`, run `cargo check --workspace` to verify resolution.
- Internal crate dependencies use workspace inheritance: `adk-core = { workspace = true }` in member crates.
- The root `Cargo.toml` `[workspace.dependencies]` section pins all internal crate versions.

## Publishing to crates.io

Always verify builds during publish — never use `--no-verify`. Verification ensures the packaged crate compiles correctly for consumers.

### Publish order

Crates must be published in dependency order. Wait for each crate to be indexed before publishing dependents.

```
Tier 1: adk-core
Tier 2: adk-telemetry, adk-memory, adk-artifact, adk-plugin, adk-skill, adk-auth, adk-guardrail,
        adk-gemini, adk-anthropic, adk-rust-macros, awp-types
Tier 3: adk-session, adk-action
Tier 4: adk-tool, adk-model, adk-browser, adk-audio
Tier 5: adk-agent, adk-graph, adk-awp, adk-acp
Tier 6: adk-runner, adk-realtime, adk-eval, adk-rag, adk-retry-reflect
Tier 7: adk-server, adk-cli, adk-deploy, adk-payments
Tier 8: cargo-adk
Tier 9: adk-rust (umbrella — always last)
```

### Publish workflow

1. Ensure all quality gates pass and the version is bumped in `[workspace.package]` and all `[workspace.dependencies]` entries.
2. Tag the release: `git tag v<version> && git push origin v<version>`.
4. Publish one crate at a time with verification:

```bash
cargo publish -p <crate-name>
```

5. Cargo waits for crates.io indexing automatically after upload. If a dependent crate fails with "failed to select a version", wait a minute and retry.
6. `adk-mistralrs` is now publishable (mistralrs on crates.io). Include it in the publish order after Tier 2.

### Version checklist

- [ ] `[workspace.package] version` in root `Cargo.toml`
- [ ] All `adk-*` entries in `[workspace.dependencies]`
- [ ] `CHANGELOG.md` updated
- [ ] Git tag created and pushed
- [ ] GitHub release created with release notes

## Crate-specific guides

- `adk-gemini/AGENTS.md` — Gemini client tracing conventions and instrumentation patterns
- `STABILITY.md` — Crate stability tiers (Stable/Beta/Experimental), deprecation policy, 1.0 milestone

## Convenience APIs (0.5.0+)

- `adk_rust::provider_from_env()` — auto-detect LLM provider from `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` / `GOOGLE_API_KEY`
- `adk_rust::run(instructions, input)` — single-function agent invocation with auto provider detection
- `Runner::builder()` — typestate builder enforcing required fields at compile time
- `Runner::run_str()` — convenience accepting `&str` for user/session IDs
- `Runner::interrupt(session_id)` — cancel a running agent mid-execution
- Prompt caching is enabled by default for Anthropic and Bedrock. Gemini explicit caching activates when `cache_capable` is set on the runner.
- Pricing modules: `adk_gemini::pricing`, `adk_model::openai::pricing`, `adk_anthropic::pricing`
- `EncryptedSession<S>` in `adk-session` (behind `encrypted-session` feature) — AES-256-GCM encryption with key rotation
- `InterruptionDetection` enum in `adk-realtime` — `Manual` (default) or `Automatic` VAD-based interruption
- `ToolSearchConfig` in `adk-anthropic` — regex-based tool filtering for the Anthropic provider
- MCP Resource API: `McpToolset::list_resources()`, `list_resource_templates()`, `read_resource(uri)`
- MCP Elicitation: `McpToolset::with_elicitation_handler()` for form/URL-based elicitation
- `McpServerManager` — lifecycle management for multiple MCP server processes with auto-restart
- Graph durable resume: `PregelExecutor` resumes from last checkpoint on startup
- `ServerBuilder` — register custom Axum controllers alongside built-in routes
- `ToolExecutionStrategy` — `Sequential`, `Parallel`, or `Auto` dispatch for tool calls
- `StatefulTool<S>` — generic wrapper for stateful tool closures
- `SimpleToolContext` — lightweight context for non-agent callers (testing, MCP servers)
- `SchemaAdapter` trait — provider-aware schema normalization (Gemini, OpenAI strict/non-strict, Anthropic, Generic)
- `parse_text_tool_calls()` — detect tool calls in text from 7 model formats (Qwen, Llama, Mistral, DeepSeek, Gemma 4, etc.)
- `SharedState` — thread-safe key-value store for parallel agent coordination
- Project-scoped memory: `MemoryService::add_session_to_project()`, `search_in_project()`

## adk-studio (separate repo)

`adk-studio` has been extracted to a standalone repository at `../adk-studio/` (or `https://github.com/zavora-ai/adk-studio`). It depends on ADK crates via local path references for development.

```bash
# Build the studio
cargo check --manifest-path ../adk-studio/Cargo.toml

# Frontend (Vite + React + TypeScript + ReactFlow)
cd ../adk-studio/ui
pnpm install          # install deps
pnpm run dev          # dev server
pnpm run build        # production build (tsc + vite)
pnpm run lint         # eslint
pnpm run test         # vitest (single run)
```
