# ADK-Rust

Rust Agent Development Kit — a modular workspace of 25 publishable crates for building AI agents with tool calling, multi-model support, real-time voice, graph workflows, and a visual studio.

## Dev environment

- Rust 1.85.0+, edition 2024. Use `make setup` or `devenv shell` to bootstrap.
- `sccache` is the compilation cache. Set `RUSTC_WRAPPER=sccache` in your shell profile.
- On Linux, `wild` is the linker (configured in `.cargo/config.toml`). macOS uses the default linker.
- Copy `.env.example` to `.env` for API keys. Never commit `.env` files or secrets.
- `adk-mistralrs` is excluded from the workspace (GPU deps). Build it explicitly: `cargo build --manifest-path adk-mistralrs/Cargo.toml`.
- `CMAKE_POLICY_VERSION_MINIMUM=3.5` is needed for cmake 4.x compatibility (audiopus).
- **Performance**: Incremental compilation is **DISABLED** (`incremental = false`) in `.cargo/config.toml` for `sccache` compatibility.

## Quality gates

Run all three before every commit. CI enforces them — save yourself the round-trip:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

### AI Agent Workflow (`devenv`)

Use these shorthand scripts instead of raw `cargo` to ensure `sccache` wrap and workspace coverage:

| Action | Command | Description |
| :--- | :--- | :--- |
| **Check** | `devenv shell check` | Fast workspace compilation check. |
| **Test** | `devenv shell test` | Run all non-ignored tests. |
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
adk-tool/        Tool system: FunctionTool, MCP integration (rmcp 0.14), Google Search
adk-runner/      Agent execution runtime with event streaming
adk-server/      HTTP server (Axum) and A2A (Agent-to-Agent) protocol
adk-session/     Session management and state persistence
adk-artifact/    Binary artifact storage for agents
adk-memory/      Semantic memory and RAG search
adk-graph/       Graph-based workflow orchestration with checkpoints and HITL
adk-realtime/    Real-time bidirectional audio/video streaming (OpenAI, Gemini Live, Vertex AI Live,
                 LiveKit, WebRTC)
adk-browser/     Browser automation tools via WebDriver
adk-eval/        Evaluation framework: trajectory, semantic, rubric, LLM-judge
adk-ui/          Dynamic UI generation: forms, cards, tables, charts, modals
adk-telemetry/   OpenTelemetry integration for agent observability
adk-guardrail/   Input/output guardrails: validation, content filtering, PII redaction
adk-auth/        Authentication: API keys, JWT, OAuth2, OIDC, SSO
adk-plugin/      Plugin system for agent lifecycle hooks
adk-skill/       Skill discovery, parsing, and convention-based agent capabilities
adk-cli/         Command-line launcher for agents
adk-studio/      Visual agent builder: Axum backend (src/) + React/ReactFlow frontend (ui/)
adk-doc-audit/   Documentation audit: rustdoc coverage, link checking, crate validation
adk-rust/        Umbrella crate re-exporting all of the above
```

### Excluded from workspace

```
adk-mistralrs/   Local LLM inference via mistral.rs (GPU deps — build explicitly)
                 Excluded so `--all-features` works without CUDA toolkit.
                 Has its own CI workflow (.github/workflows/mistralrs-tests.yml).
```

### Examples and docs

```
examples/              120+ working examples organized by provider/feature
examples/ralph/        Standalone autonomous agent crate (own Cargo.toml + tests/)
docs/official_docs/    Comprehensive documentation site content
docs/official_docs_examples/  Compilable code snippets validating every doc page
```

## Feature flags

### adk-model

`gemini` is the default. All others are opt-in:

- `gemini` (default), `openai`, `anthropic`, `deepseek`, `ollama`, `groq`
- `fireworks`, `together`, `mistral`, `perplexity`, `cerebras`, `sambanova` — OpenAI-compatible providers
- `bedrock` — Amazon Bedrock via AWS SDK Converse API
- `azure-ai` — Azure AI Inference endpoints
- `all-providers` — enables all fourteen

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

`full` is the default, which enables all component features. Individual features like `agents`, `models`, `tools`, `sessions`, `runner`, `server`, `graph`, `realtime`, `eval`, `browser`, `auth`, `guardrail`, `plugin`, `telemetry`, `cli`, `ui`, `doc-audit`, `skills`, `artifacts`, `memory` can be selected individually.

## Rust conventions

- Always use `thiserror` for library error types. Include actionable guidance in error messages.
- Always use `async-trait` for async trait methods.
- Always use `tracing` for logging. Never `println!` or `eprintln!` in library code.
- Always use `Arc<T>` for shared ownership across async boundaries.
- Always use `tokio::sync::RwLock` for async-safe interior mutability.
- Use builder pattern for complex configuration.
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
cargo test --workspace                                    # full workspace
cargo test -p adk-core                                    # single crate
cargo test -p adk-realtime --features full                # with features
cargo test -p ralph                                       # standalone example crate
```

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
4. Add example under `examples/<provider>_basic/`
5. Update `adk-studio` codegen if the provider should be available in Studio

### New tool

1. Implement `adk_core::Tool` trait (use `Arc<dyn ToolContext>` for context parameter)
2. Add to `adk-tool/src/` or new crate if heavy deps
3. Write unit + property tests

### New example

Simple: `examples/<name>/main.rs` + `[[example]]` entry in `examples/Cargo.toml`.

Complex (own deps/tests): standalone crate at `examples/<name>/` with own `Cargo.toml`, add to `[workspace.members]` in root `Cargo.toml`. See `examples/ralph/` as the reference.

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
- `adk-plugin` has a hardcoded `adk-core` dep (not workspace ref) — keep it in sync manually.

## Publishing to crates.io

Always verify builds during publish — never use `--no-verify`. Verification ensures the packaged crate compiles correctly for consumers.

### Publish order

Crates must be published in dependency order. Wait for each crate to be indexed before publishing dependents.

```
Tier 1: adk-core
Tier 2: adk-telemetry, adk-memory, adk-artifact, adk-plugin, adk-skill, adk-auth, adk-guardrail, adk-gemini
Tier 3: adk-session
Tier 4: adk-tool, adk-model, adk-ui, adk-browser
Tier 5: adk-agent, adk-graph
Tier 6: adk-runner, adk-realtime, adk-eval
Tier 7: adk-server, adk-cli, adk-studio, adk-doc-audit
Tier 8: adk-rust (umbrella — always last)
```

### Publish workflow

1. Ensure all quality gates pass and the version is bumped in `[workspace.package]` and all `[workspace.dependencies]` entries.
2. Bump `adk-plugin/Cargo.toml` manually — it has a hardcoded `adk-core` dep.
3. Tag the release: `git tag v<version> && git push origin v<version>`.
4. Publish one crate at a time with verification:

```bash
cargo publish -p <crate-name>
```

5. Cargo waits for crates.io indexing automatically after upload. If a dependent crate fails with "failed to select a version", wait a minute and retry.
6. Skip `adk-mistralrs` — it is `publish = false`.

### Version checklist

- [ ] `[workspace.package] version` in root `Cargo.toml`
- [ ] All 23 `adk-*` entries in `[workspace.dependencies]`
- [ ] `adk-plugin/Cargo.toml` hardcoded `adk-core` version
- [ ] `CHANGELOG.md` updated
- [ ] Git tag created and pushed
- [ ] GitHub release created with release notes

## Crate-specific guides

- `adk-gemini/AGENTS.md` — Gemini client tracing conventions and instrumentation patterns

## adk-studio (frontend)

The `adk-studio/ui/` directory is a Vite + React + TypeScript app using ReactFlow. It has its own toolchain:

```bash
cd adk-studio/ui
pnpm install          # install deps
pnpm run dev          # dev server
pnpm run build        # production build (tsc + vite)
pnpm run lint         # eslint
pnpm run test         # vitest (single run)
```

When making changes to the Studio frontend, run `pnpm run lint` and `pnpm run test` before committing. The Rust backend in `adk-studio/src/` follows the normal Cargo quality gates.
