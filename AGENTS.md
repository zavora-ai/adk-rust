# ADK-Rust

Rust Agent Development Kit — a modular workspace of publishable crates for building AI agents with tool calling, multi-model support, real-time voice, graph workflows, and more.

## Dev environment

- Rust 1.85.0+, edition 2024. Use `make setup` or `devenv shell` to bootstrap.
- `sccache` is the compilation cache. Set `RUSTC_WRAPPER=sccache` in your shell profile.
- On Linux, `wild` is the linker (configured in `.cargo/config.toml`). macOS uses the default linker.
- Copy `.env.example` to `.env` for API keys. Never commit `.env` files or secrets.
- `adk-mistralrs` is excluded from the workspace (GPU deps). Build it explicitly: `cargo build --manifest-path adk-mistralrs/Cargo.toml`.
- `CMAKE_POLICY_VERSION_MINIMUM=3.5` is needed for cmake 4.x compatibility (audiopus).
- **Performance**: `.cargo/config.toml` sets `incremental = false` globally for `sccache` compatibility, but `Cargo.toml` `[profile.dev]` overrides this with `incremental = true` for local dev builds. CI uses the `ci` profile where the config.toml setting applies.

## Quality gates

Run all three before every commit. CI enforces them — save yourself the round-trip:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

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
adk-core/        Core traits and types: Agent, Tool, Llm, Session, Event, Content, State
adk-agent/       Agent implementations: LlmAgent, CustomAgent, SequentialAgent, ParallelAgent,
                 LoopAgent, ConditionalAgent, LlmConditionalAgent
adk-model/       LLM provider facade: Gemini, OpenAI, Anthropic, DeepSeek, Groq, Ollama,
                 Fireworks, Together, Mistral, Perplexity, Cerebras, SambaNova, Bedrock, Azure AI
                 (feature-gated)
adk-gemini/      Dedicated Gemini client with GeminiBackend trait (Studio + Vertex AI)
adk-tool/        Tool system: FunctionTool, MCP integration (rmcp 1.2), Google Search, MCP Resource API
adk-runner/      Agent execution runtime with event streaming
adk-server/      HTTP server (Axum) and A2A (Agent-to-Agent) protocol
adk-session/     Session management and state persistence, encrypted sessions (AES-256-GCM)
adk-artifact/    Binary artifact storage for agents
adk-memory/      Semantic memory and RAG search
adk-graph/       Graph-based workflow orchestration with checkpoints, durable resume, and HITL
adk-realtime/    Real-time bidirectional audio/video streaming (OpenAI, Gemini Live, Vertex AI Live,
                 LiveKit, WebRTC), mid-session context mutation, interruption detection
adk-browser/     Browser automation tools via WebDriver
adk-eval/        Evaluation framework: trajectory, semantic, rubric, LLM-judge
adk-telemetry/   OpenTelemetry 0.31 integration for agent observability
adk-guardrail/   Input/output guardrails: validation, content filtering, PII redaction
adk-auth/        Authentication: API keys, JWT, OAuth2, OIDC, SSO
adk-plugin/      Plugin system for agent lifecycle hooks
adk-skill/       Skill discovery, parsing, and convention-based agent capabilities
adk-cli/         Command-line launcher for agents
adk-anthropic/   Dedicated Anthropic API client with streaming, thinking, caching, citations, pricing
adk-doc-audit/   Documentation audit: rustdoc coverage, link checking, crate validation
adk-rust-macros/ Procedural macros (#[tool] attribute)
adk-code/        Code execution (experimental)
adk-sandbox/     Sandboxed execution environments (experimental)
adk-audio/       Audio processing, STT/TTS providers, Deepgram streaming (experimental)
adk-rag/         Retrieval-augmented generation pipelines
adk-action/      Action node execution for deterministic workflow operations
adk-deploy/      Deployment utilities
adk-payments/    Payment integration for agent services
cargo-adk/       Cargo subcommand for project scaffolding
adk-rust/        Umbrella crate re-exporting all of the above
```

### Excluded from workspace

```
adk-mistralrs/   Local LLM inference via mistral.rs (GPU deps — build explicitly)
                 Excluded so `--all-features` works without CUDA toolkit.
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
examples/              README pointing to adk-playground repo (120+ examples)
docs/official_docs/    Comprehensive documentation site content
```

## Feature flags

### adk-model

`gemini` is the default. All others are opt-in:

- `gemini` (default), `openai`, `anthropic`, `deepseek`, `ollama`, `groq`
- `openrouter` — OpenRouter native chat, responses, routing, discovery, and credits APIs
- `bedrock` — Amazon Bedrock via AWS SDK Converse API
- `azure-ai` — Azure AI Inference endpoints
- `all-providers` — enables all eight real feature flags
- `fireworks`, `together`, `mistral`, `perplexity`, `cerebras`, `sambanova`, `xai` — backward-compat aliases for `openai`. Use `OpenAICompatibleConfig` presets instead of separate client types.

### adk-realtime

All opt-in, no defaults:

- `openai` — OpenAI Realtime API (WebSocket)
- `gemini` — Gemini Live API (WebSocket)
- `vertex-live` — Vertex AI Live API (OAuth2 via ADC, implies `gemini`)
- `livekit` — LiveKit WebRTC bridge
- `openai-webrtc` — OpenAI WebRTC transport (implies `openai`, requires cmake)
- `full` — All providers except WebRTC (no cmake needed)
- `full-webrtc` — Everything including WebRTC (requires cmake)

### adk-rust (umbrella)

Four presets control which crates are compiled:

- `standard` **(default)** — agents, models, gemini, anthropic, tools, skills, sessions, artifacts, memory, runner, telemetry, guardrail, auth, plugin. Everything needed to build and run agents.
- `full` — standard + graph, realtime, browser, eval, rag. All stable specialist crates. Does **not** include experimental crates.
- `labs` — standard + code, sandbox, audio. Experimental crates that may have unstable APIs.
- `minimal` — agents, gemini, runner. Fastest possible build.

To compile everything (stable + experimental): `features = ["full", "labs"]`.

Individual features (`agents`, `models`, `tools`, `sessions`, `server`, `graph`, `realtime`, `eval`, `browser`, `auth`, `guardrail`, `plugin`, `telemetry`, `cli`, `skills`, `artifacts`, `memory`, `code`, `sandbox`, `audio`, etc.) can be selected independently.

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

When implementing `Llm`:
- `generate_content()` takes `LlmRequest` and `bool` (stream flag), returns `Result<LlmResponseStream>`.

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
- Doc examples: validated via `docs/official_docs_examples/` workspace members.
- When writing tests, prefer comparing equality of entire objects over fields one by one.
- Tests that require API keys or external services should be `#[ignore]`.
- Run the test for the specific crate you changed first (`cargo test -p adk-<crate>`), then run the full workspace if you changed shared crates like `adk-core`.
- Crate-specific or individual tests can be run without asking the user, but ask before running the full workspace test suite.

## Documentation

- When making a change that adds or changes an API, ensure that `docs/official_docs/` is up to date.
- Doc examples in `docs/official_docs_examples/` are compiled as workspace members in CI. If you change a public API, the corresponding doc example must still compile.
- Update the crate's `README.md` if capabilities changed.
- Update `CHANGELOG.md` for user-facing changes.

## Adding new code

### New LLM provider

1. Implement `adk_core::Llm` in `adk-model/src/<provider>/`
2. Add feature flag to `adk-model/Cargo.toml`
3. Re-export from `adk-model/src/lib.rs` behind the feature
4. Add example in [adk-playground](https://github.com/zavora-ai/adk-playground)
5. Update `adk-studio` codegen if the provider should be available in Studio (separate repo: `../adk-studio/`)

### New tool

1. Implement `adk_core::Tool` trait (use `Arc<dyn ToolContext>` for context parameter)
2. Add to `adk-tool/src/` or new crate if heavy deps
3. Write unit + property tests

### New example

Add examples to the [adk-playground](https://github.com/zavora-ai/adk-playground) repo. The `examples/` directory in this workspace contains only a README pointing there.

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
- `adk-plugin` now uses workspace inheritance for `adk-core` (previously hardcoded).

## Publishing to crates.io

Always verify builds during publish — never use `--no-verify`. Verification ensures the packaged crate compiles correctly for consumers.

### Publish order

Crates must be published in dependency order. Wait for each crate to be indexed before publishing dependents.

```
Tier 1: adk-core
Tier 2: adk-telemetry, adk-memory, adk-artifact, adk-plugin, adk-skill, adk-auth, adk-guardrail, adk-gemini, adk-anthropic, adk-rust-macros
Tier 3: adk-session
Tier 4: adk-tool, adk-model, adk-browser, adk-audio
Tier 5: adk-agent, adk-graph, adk-action
Tier 6: adk-runner, adk-realtime, adk-eval, adk-rag
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
6. Skip `adk-mistralrs` — it is `publish = false`.

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
- Prompt caching is enabled by default for Anthropic and Bedrock. Gemini explicit caching activates when `cache_capable` is set on the runner.
- Pricing modules: `adk_gemini::pricing`, `adk_model::openai::pricing`, `adk_anthropic::pricing`
- `EncryptedSession<S>` in `adk-session` (behind `encrypted-session` feature) — AES-256-GCM encryption with key rotation
- `InterruptionDetection` enum in `adk-realtime` — `Manual` (default) or `Automatic` VAD-based interruption
- `ToolSearchConfig` in `adk-anthropic` — regex-based tool filtering for the Anthropic provider
- MCP Resource API: `McpToolset::list_resources()`, `list_resource_templates()`, `read_resource(uri)`
- Graph durable resume: `PregelExecutor` resumes from last checkpoint on startup

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
