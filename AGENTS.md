# ADK-Rust

Rust Agent Development Kit — a modular workspace of publishable crates for building AI agents with tool calling, multi-model support, real-time voice, graph workflows, and more.

## Dev environment

- Rust 1.95.0+, edition 2024, pinned by `rust-toolchain.toml` — rustup installs it on the
  first `cargo` invocation, so do not pass `+stable` or `+nightly`.
- New clone? See [First-time setup](#first-time-setup) for the per-platform tool matrix.
- `sccache` is the compilation cache. Set `RUSTC_WRAPPER=sccache` in your shell profile.
- Linkers are set in `.cargo/config.toml`: `wild` on Linux (devenv-provided), the
  default on macOS, `rust-lld` on Windows MSVC.
- Copy `.env.example` to `.env` for API keys. Never commit `.env` files or secrets.
- `adk-mistralrs` is a workspace member using workspace version inheritance. GPU features (`cuda`, `metal`) are opt-in.
- **Performance**: `.cargo/config.toml` sets `incremental = false` globally for `sccache` compatibility, but `Cargo.toml` `[profile.dev]` overrides this with `incremental = true` for local dev builds. CI uses the `ci` profile where the config.toml setting applies.

### First-time setup

`devenv shell` is the reproducible path on Linux and macOS and needs nothing below.
Everything else runs one setup script, which is idempotent and has a
report-only mode:

```bash
./scripts/setup-dev.sh            # Linux, macOS
./scripts/setup-dev.sh --check    # report without changing anything
```

```powershell
./scripts/setup-dev.ps1           # Windows
./scripts/setup-dev.ps1 -Check    # report without changing anything
```

Both scripts check the toolchain, `cargo-nextest`, `protoc`, NASM, `cmake`, and
lefthook, then report the environment variables the build expects. The Windows
script additionally verifies `bash` and `python3` resolve, installs `protoc`,
NASM, and nextest from prebuilt archives into `~/.local`, and persists
`RUSTC_WRAPPER`, `CMAKE_POLICY_VERSION_MINIMUM`, and `PROTOC` for the user.
Tools with no unattended installer (Visual Studio Build Tools, Git for Windows,
Python) are reported with a download link.

CONTRIBUTING.md ("Dev Environment Setup") is the contributor-facing walkthrough;
the rest of this section is the per-platform tool matrix and the reason each tool
exists.

**Build tools.** Which ones you need depends on the features you build. A
default `cargo check --workspace` needs none of them; the rows below are what the
feature-gated builds and the `full` tier require.

| Tool | Needed for | Linux | macOS | Windows |
|------|-----------|-------|-------|---------|
| `protoc` | `adk-rag --features lancedb` (lance build script) | `apt install protobuf-compiler` | `brew install protobuf` | [prebuilt release](https://github.com/protocolbuffers/protobuf/releases), then set `PROTOC` |
| NASM | `aws-lc-sys` on MSVC — any rustls path, e.g. `adk-auth --features sso` | not needed | not needed | [nasm.us](https://www.nasm.us/), add to `PATH` |
| `cmake` | `openai-webrtc` (audiopus builds Opus from source) | `apt install cmake` | `brew install cmake` | [get-cmake](https://github.com/lukka/get-cmake) or the official installer |
| ALSA (`libasound2-dev`) | `examples/desktop_audio` (cpal links `alsa-sys`); pre-push examples gate | `apt install libasound2-dev` | not needed (CoreAudio) | not needed |
| `bash` / `sh` | Shell-tool and external-runner tests across `adk-bench`, `adk-devtools`, `adk-managed`, `adk-sandbox`, `adk-tool` | preinstalled | preinstalled | [Git for Windows](https://git-scm.com/download/win) ships them in `Git\bin`, which the installer leaves off `PATH` — add it |
| `python3` | `adk-sandbox` process-execution tests | preinstalled | preinstalled | [python.org](https://www.python.org/downloads/) — ensure `python3` resolves, not just `python` |
| WebDriver | `adk-browser` examples | Chrome/Chromium | Chrome/Chromium | Chrome/Chromium |

CI installs `protoc` on every `feature-coverage` runner and sets `PROTOC=protoc`
(see `.github/workflows/ci.yml`), so a missing `protoc` fails locally while CI
stays green. The same applies to NASM on Windows.

**Environment variables.** Both setup scripts report these; the PowerShell one
persists them.

| Variable | Value | Why |
|----------|-------|-----|
| `RUSTC_WRAPPER` | `sccache` | Compilation cache; cuts rebuild times substantially |
| `CMAKE_POLICY_VERSION_MINIMUM` | `3.5` | cmake 4.x compatibility (audiopus) |
| `PROTOC` | path to `protoc` | Only when `protoc` is not on `PATH` |

```bash
# Linux / macOS — add to ~/.bashrc or ~/.zshrc
export RUSTC_WRAPPER=sccache
export CMAKE_POLICY_VERSION_MINIMUM=3.5
```

```powershell
# Windows — persist for the current user
setx RUSTC_WRAPPER sccache
setx CMAKE_POLICY_VERSION_MINIMUM 3.5
```

**Git hooks.** Both setup scripts install lefthook and run `lefthook install`.
Lefthook is not on crates.io, so the Windows script uses npm and falls back to
pointing at the [prebuilt releases](https://github.com/evilmartians/lefthook/releases).

### Platform notes

**Linux.** `.cargo/config.toml` pins `wild` as the linker for Linux targets, and
only the devenv/nix shell provides it. Outside that shell, either install `wild`
or strip the three `[target.*]` linker sections — the shared
`.github/actions/setup-rust` action does the latter on plain runners
(`neutralize-wild`) and is the reference for which sections to remove.

**macOS.** Uses the default linker; no linker setup required.

**Windows.** `make setup`, `./scripts/setup-dev.sh`, and `devenv shell` are all
bash-only, so use `./scripts/setup-dev.ps1`. Use the MSVC toolchain
(`x86_64-pc-windows-msvc`) with Visual Studio Build Tools for the C/C++
compiler; `.cargo/config.toml` pins `rust-lld` as the linker, which ships with
rustup. Four further constraints:

- `adk-sandbox`'s Windows enforcer is a restricted-token tier that confines only
  through `WindowsEnforcer::run_confined`; its `probe` reports the generic
  `sandboxed_command` path unavailable, so the harness shell stays degraded.
- Nine tests shell out to `bash`, `sh`, or `python3` and fail with
  `program not found` without them. Git for Windows installs a shell but leaves
  `C:\Program Files\Git\bin` off `PATH` (only `Git\cmd` is added), and the
  python.org installer creates no `python3.exe`. Add `Git\bin` to `PATH` and copy
  `python.exe` to `python3.exe` in your Python directory.
- Run the suite from a Developer PowerShell, or `call vcvars64.bat` first.
  `adk-sandbox` forwards `LIB` from the parent environment so `rustc` can reach
  the MSVC linker (`ProcessBackend::toolchain_env`), so
  `a_working_program_still_runs` fails in a plain shell where `LIB` is unset.
- Keep Git's `usr/bin` off `PATH` ahead of the toolchain: it ships a GNU
  `link.exe` that shadows the MSVC linker, which once made sandboxed Rust
  compilation resolve the wrong one. `adk-bench`'s external-runner tests run
  their scripts with Windows PowerShell 5.1 and need nothing from Git.

### Verifying a fresh setup

Start with the three [quality gates](#quality-gates), the full workspace suite included.
A default `--workspace` build compiles no feature-gated module and no umbrella
tier above `minimal`, so those need their own pass:

```bash
# Feature-gated modules — one pair from scripts/feature-coverage-pairs.txt
cargo clippy -p adk-agent --features codeact --all-targets -- -D warnings

# Umbrella tiers — `standard` and above compile code `minimal` never reaches
cargo clippy -p adk-rust --features full --all-targets -- -D warnings

# Doctests — nextest does not run them
cargo test -p adk-rust --features full --doc
```

A clean setup runs the full suite with zero failures on Linux, macOS, and
Windows — on Windows from a Developer PowerShell, as
[Platform notes](#platform-notes) explains. Any failure carrying `program not found` is a
missing host tool, not a regression — diagnose the environment before assuming
the workspace is broken.

## Quality gates

The PR tier runs all three on every pull request:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

Before every commit, run the first two and `cargo nextest run -p <crate>` for each crate you
changed; [Testing](#testing) says when to run the full workspace suite.

### CI cost tiers

CI is organized into cost tiers so per-PR feedback stays fast while full coverage
is preserved. No coverage is dropped —
expensive axes move to a later tier.

- **PR tier** (`ci.yml` + `semver.yml`, on pull requests) — the merge-blocking
  coverage: `fmt` (prerequisite gate), `clippy --workspace --all-targets -D warnings`,
  `nextest --workspace` (Linux, runs at most once), `feature-coverage` for
  feature-gated modules default builds skip (e.g. `adk-agent --features codeact`),
  as eleven shards driven by `scripts/feature-coverage-pairs.txt`, `postgres` (the
  `#[ignore]`d PostgreSQL suites, pgvector and TLS included, against a
  `pgvector/pgvector:pg17` service, driven by `scripts/test-postgres.sh`),
  `docs` (`cargo doc --workspace --no-deps` plus doctests), standalone examples
  (4 shards), `kernel-scenarios` (the scripted scenarios of `examples/harness_kernel`, on Linux),
  `website-scenarios` (spec 3.5's scripted scenarios of `examples/website`),
  `templates`, a compile-only macOS build, a Windows workspace build
  with targeted sandbox portability smokes and the kernel's crate-level scenarios
  (`adk-kernel`'s K2–K4, K6, K8 and K9), and `semver` (stable strict against
  the release type in `semver.yml`'s `SEMVER_RELEASE_TYPE`: `minor` fails breakage,
  `major` during a major release candidate accepts it;
  everything else warn-only). A `scope` job skips the Rust jobs when a change
  touches only `docs/` or Markdown, and merge-queue builds skip `docs`, the
  standalone examples, the kernel scenarios and the macOS/Windows builds the pull
  request already passed.
- **Merge tier** (`ci-merge.yml`, on `push: main` and on demand) — cross-platform
  `nextest --workspace` on macOS/Windows, the scripted scenarios of
  `examples/harness_kernel` on macOS (`kernel-scenarios-macos`; the PR tier's macOS
  build is compile-only), and doc-example compilation. Runs
  post-merge, or on a branch with `gh workflow run ci-merge.yml --ref <branch>`;
  not branch-protection-required. `sandbox-linux.yml` (on `push: main`,
  on a pull request touching the sandbox or the harness shell, and on demand) proves
  bubblewrap enforcement on `ubuntu-24.04` with `ADK_SANDBOX_REQUIRE_ENFORCER=1`, under
  which the enforcement tests fail instead of skipping when the enforcer cannot start.
- **Nightly tier** (`ci-nightly.yml`, on `schedule`) — the feature-combination
  matrix (clippy with `-D warnings`), `cargo-audit`/`cargo-deny` supply-chain
  checks, and `#[ignore]` integration tests gated on available secrets. Not
  branch-protection-required.

Only the PR tier gates merges. Branch protection requires the aggregate
`pr-gate` context plus the separate `semver` context; `pr-gate` fails unless
every `ci.yml` dependency succeeds or was skipped by the change scope. See CONTRIBUTING.md
("Branch Protection — Required Status Checks") for the authoritative set.

### Local git hooks (lefthook)

- **pre-commit** — `cargo fmt --all -- --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `scripts/check-feature-growth.sh` (a new
  `adk-rust` feature must change what links or carry a recorded decision — see
  [docs/feature-surface.md](docs/feature-surface.md)), `shellcheck` on staged
  shell scripts, and PSScriptAnalyzer on staged `*.ps1` via
  `scripts/lint-powershell.ps1` (skips itself when the module is not installed).
- **pre-push** — `cargo check --workspace` (a fast compilation check, not the full
  test suite), plus `scripts/check-examples-compile.sh` over all four shards when
  an example or a workspace crate it depends on has changed. The standalone
  examples are outside the workspace, so nothing else compiles them. CI is the
  full-suite safety net, so the local gate stays quick.

### AI Agent Workflow (`devenv`)

Inside `devenv shell`, these scripts run the same commands with `sccache`:

| Action | Command | Runs |
| :--- | :--- | :--- |
| **Check** | `devenv shell ws-check` | `cargo check --workspace` |
| **Test** | `devenv shell ws-test` | `cargo nextest run --workspace` — the full suite |
| **Lint** | `devenv shell ws-clippy` | `cargo clippy --workspace --all-targets -- -D warnings` |
| **Format** | `devenv shell ws-fmt` | `cargo fmt --all` |

Run `cargo fmt --all` automatically after finishing Rust code changes; do not ask for approval.

Clippy runs with `-D warnings` — every warning is a compile error. Fix warnings before pushing. Don't suppress with `#[allow(...)]` unless there's a documented reason.

## Workspace structure

Crate names are prefixed with `adk-`. The Rust module name uses underscores (`adk_core`, `adk_agent`, etc.).

### Core crates (publishable to crates.io)

One line per crate — the crate's README holds the detail.

```
adk-core/          Core traits and types, `AdkError`, the governed tool path, identity, durable contracts
adk-agent/         LlmAgent, workflow agents, teams, CodeActAgent (`codeact`), CodingAgent, AmbientAgent
adk-model/         LLM providers: Gemini, OpenAI, Anthropic, DeepSeek, Groq, Ollama, Bedrock, Azure, OpenRouter
adk-gemini/        Gemini client: Studio and Vertex backends, built-in tools, Interactions API (`interactions`)
adk-gcp/           Google Cloud REST plumbing: ADC credentials, bounded HTTP transport, LRO polling
adk-anthropic/     Anthropic client: streaming, thinking, prompt caching, Managed Agents and Files APIs
adk-tool/          FunctionTool, MCP client and server, built-in tool wrappers, Slack/BigQuery/Spanner
adk-runner/        Execution runtime: event streaming, budgets, ledgers, cancellation and recovery
adk-server/        Axum server, A2A v1.0.0, background runs and cron (`background`), AWP edge (`awp`)
adk-session/       Sessions on SQLite, PostgreSQL, Redis, MongoDB, Firestore, Neo4j, Vertex; durable stores
adk-artifact/      Versioned binary artifacts: in-memory, file, GCS (`gcs`)
adk-memory/        Semantic memory and search on seven backends, SQLite knowledge graph (`graph-memory`)
adk-graph/         Graph workflows: checkpoints, durable resume, interrupts, action nodes, functional API
adk-realtime/      Realtime audio/video: OpenAI, Gemini Live, Vertex Live, LiveKit, WebRTC, avatars
adk-browser/       Browser automation tools over WebDriver
adk-computer-use/  Governed graph, auth and wire contracts for computer-use-mcp; performs no actuation
adk-eval/          Evaluation: trajectory, rubric, LLM judge, personas, baselines, experiments
adk-telemetry/     OpenTelemetry 0.32: OTLP, SQLite and GCP export, GenAI semantic conventions
adk-guardrail/     Input, output, tool-call and tool-result guardrails, PII redaction
adk-auth/          API keys, JWT, OAuth2/OIDC/SSO, secret providers, RBAC, signed audit, Web Bot Auth
adk-plugin/        EnhancedPlugin lifecycle hooks with tool and model interception
adk-retry-reflect/ Retry & Reflect plugin: reflection prompts, backoff, circuit breaking on tool failure
adk-skill/         AgentSkills parsing, index and injection; Vertex Skill Registry client
adk-cli/           The `adk-rust` binary (REPL, server, harness, website, site, awp) and `Launcher`
adk-rust-macros/   `#[tool]`, and `#[entrypoint]`/`#[task]` for the graph functional API
adk-code/          Code execution substrate: executors, sandbox policy, Vertex sandbox (`vertex-sandbox`)
adk-codeact-monty/ Python `CodeRuntime` for CodeActAgent on the Monty interpreter
adk-devtools/      Coding-agent file, search and edit tools scoped to a sandboxed workspace
adk-harness/       Governed harness agent: modes, standing approvals, shell, goals, memory, presets
adk-kernel/        Agent kernel: supervisor, process table, wake sources, event bus, control plane
adk-surface/       Agent surfaces: SurfaceEngine, web API (`web`), agent-drawn UI (`genui`)
adk-site/          AWP website pipeline: content, generator, launch gate, signed releases, hosts
adk-drivers/       Website service drivers (DNS, email, analytics, social) with conformance suites
adk-website/       The prebuilt AWP website product and the `adk-website` binary
adk-bench/         Runtime benchmarks with real LLM APIs and a Python ADK comparison
adk-sandbox/       Process/WASM sandboxes; Seatbelt, bubblewrap and Windows restricted-token enforcers
adk-audio/         STT/TTS providers, desktop audio, ONNX models (Whisper, Kokoro, ...)
adk-rag/           Retrieval-augmented generation pipelines and vector store backends
adk-action/        Action node types and variable interpolation for adk-graph
adk-enterprise/    HTTP/SSE client for the ADK-Rust Enterprise Managed Agent Service
adk-managed/       Provider-neutral managed agent runtime with durable, parked turns
adk-deploy/        Deployment manifests, bundling, and the Agent Engine client (`gcp`)
adk-payments/      Agentic commerce (ACP, AP2) and the owner-funded wallet (`wallet`)
cargo-adk/         `cargo adk`: project templates, builds, eval runs, deployment
adk-mistralrs/     In-process local inference via mistral.rs 0.8; GPU features opt-in
adk-rust/          Umbrella crate: re-exports everything behind tiered features
```

### Protocol crates

```
awp-types/         AWP 1.x and 2.0 protocol types and JSON Schemas; no adk-* dependencies
adk-awp/           AWP implementation: /awp/v2/ routes, `signing`, `verify`, the 1.x routes (`awp-v1`)
adk-acp/           Agent Client Protocol: ACP agents as tools, and ADK agents served over ACP
```

### Excluded from workspace

```
adk-studio/      Retired in favour of adk-surface's workflow observer
                 (docs/official_docs/surfaces/workflow.md).
                 Repo: https://github.com/zavora-ai/adk-studio

adk-ui/          Retired, for ADK-Rust 2 only (spec 3.3 decision 11): agent-drawn UI is
                 adk-surface's `genui` feature. Do not add code to it or depend on it.
                 Repo: https://github.com/zavora-ai/adk-ui
                 UI protocol constants stay in adk-server/src/ui_protocol.rs for the
                 deprecated `adk_ui` profile and the /api/ui bridge.
```

### Examples and docs

```
examples/              Standalone example crates, each with its own Cargo.toml and outside the
                       workspace. More examples live in the adk-playground repo.
docs/official_docs/    Comprehensive documentation site content
```

## Feature flags

### adk-model

`gemini` is the default. All others are opt-in:

- `gemini` (default), `openai`, `anthropic`, `deepseek`, `ollama`, `groq`, `muse`
- `gemini-vertex` — Gemini via Vertex AI (Studio + Vertex backends)
- `gemini-interactions` — Gemini Interactions API (Beta); re-exports `adk_model::gemini::interactions` AND provides the runtime transport toggle on `GeminiModel` (`use_interactions_api`) that drives the standard `LlmAgent`/`Runner`/tool loop through the Interactions endpoint (allowlist, stateful continuity, `bypass_multi_tools_limit`)
- `openrouter` — OpenRouter native chat, responses, routing, discovery, and credits APIs
- `opencode` — OpenCode Go and Zen: `OpenCodeClient` routes each model through Chat Completions, Responses, Anthropic Messages, or Gemini `generateContent` with the application's identity headers (implies `openai`, `anthropic`, `gemini`)
- `bedrock` — Amazon Bedrock via AWS SDK Converse API
- `azure-ai` — Azure AI Inference endpoints
- `all-providers` — enables all real feature flags
- `fireworks`, `together`, `mistral`, `perplexity`, `cerebras`, `sambanova`, `xai` — backward-compat aliases for `openai`. Use `OpenAICompatibleConfig` presets instead of separate client types.

### Gemini model selection

Use current-generation models. Gemini 2.0 models were shut down on June 1, 2026.

| Use case | Model ID | Notes |
|----------|----------|-------|
| Default / general | `gemini-3.7-flash` | Default in `adk-gemini` and `adk-model` |
| Cost-efficient / high-volume | `gemini-3.5-flash-lite` | Fast routing and high-volume work |
| Advanced reasoning | `gemini-3.1-pro-preview` | Preview reasoning model |
| Image generation | `gemini-3.1-flash-image` | Native image output |
| Live voice | `gemini-3.1-flash-live-preview` | Realtime audio and video |

**Avoid retired models:** `gemini-2.0-flash`, `gemini-2.0-flash-lite`,
`gemini-3.1-flash-lite-preview`, and `gemini-3-pro-preview`.

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

- `minimal` **(default)** — agents, models, gemini, runner, sessions.
- `standard` — minimal + openai, anthropic, anthropic-client, tools, memory, telemetry, skills, graph, auth, server, eval, guardrail, plugin, artifacts, computer-use.
- `enterprise` — standard + realtime, browser, rag, payments, awp.
- `full` — enterprise + audio, code, sandbox, code-tools, graph-functional, graph-node-cache, graph-delta, graph-time-travel, harness, harness-kernel, website. Everything that needs no external infrastructure.

Every other feature composes with any tier (`features = ["minimal", "audio"]`).
`adk-rust/Cargo.toml` lists each one with what it forwards to; production backends
(`postgres-session`, `graph-postgres`, ...) need external infrastructure and are in no tier.
A new feature must change what links or carry a recorded decision — see
[docs/feature-surface.md](docs/feature-surface.md).

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
- `screen_user_input()` defaults to screening with `sub_agents()`. The runner calls it on the root
  before saving a user message, so an agent with input guardrails applies them there, and a
  wrapper whose inner agent is not in `sub_agents()` delegates to it.

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

Five mechanisms control tool execution and compose in any combination:

| Mechanism | Set with | Decides |
|-----------|----------|---------|
| `ToolPolicy` | `Runner::builder().tool_policy(..)`, `RunConfig::tool_policy` | Allow, deny, or require approval per call; `DeclarativePolicy` denies any tool no rule permits |
| `ToolConfirmationPolicy` | `.require_tool_confirmation(name)` on the agent builder | Human approval — a `ToolConfirmationHandler` in the run, or a held call a later run resumes |
| `BeforeToolCallback` | `.before_tool_callback(..)` | Skips a call by returning content |
| `adk-auth` RBAC | `ProtectedTool`, `ScopeGuard` wrappers | Role and scope checks inside `execute()`, as a tool error |
| Graph interrupts | `adk-graph` | Checkpointed pauses for multi-step approval workflows |

`GovernanceControl` is the kill switch over all of them: `runner.governance().freeze(reason)`,
or `POST /api/admin/freeze` on `adk-server`. A confirmation decision binds to the call's
fingerprint — acting agent, tool origin, tool name, and canonical arguments — not its call ID.

`adk_core::authorize_tool_call` is the one governed path. `LlmAgent`
(`adk-agent/src/llm_agent.rs`), `CodeActAgent`, and the realtime runners dispatch through it, and
`adk-agent/src/tool_ledger.rs` is the shared action-ledger step. A change to the tool path keeps
both agents in step and updates `docs/official_docs/security/tool-authorization.md`, whose
"Combining Mechanisms" and "Resuming a Held Call" sections are the reference for evaluation order,
held calls, and resume. Every run carries an acting agent (`ActingContext`) — see
`docs/official_docs/security/agent-identity.md`; context wrappers must forward `acting_context()`.

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
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres \
  scripts/test-postgres.sh                                     # PostgreSQL suites (pgvector server)
```

- Prefer `cargo nextest run` over `cargo test` for speed (~10x faster via parallel test binary execution).
- Use `cargo test` only for doctests (nextest doesn't run them) or when you need `--doc` specifically.

- Unit tests: `#[cfg(test)]` modules in source files.
- Integration tests: `tests/*.rs` in each crate.
- Property tests: `tests/*_property_tests.rs` using `proptest` with 100+ iterations.
- Doc examples: Cargo snippets, feature names, and package/example references in `README.md` and `docs/official_docs/` are validated against `cargo metadata` by `scripts/check-doc-examples.sh`.
- When writing tests, prefer comparing equality of entire objects over fields one by one.
- Tests that require API keys or external services should be `#[ignore]`.
- Run the tests of each crate you changed (`cargo nextest run -p adk-<crate>`) without asking.
- Ask before running the full workspace suite, even after changing a shared crate such as `adk-core`;
  the PR tier runs `nextest --workspace` on every pull request.

## Documentation

- When making a change that adds or changes an API, ensure that `docs/official_docs/` is up to date.
- Documented examples (Cargo snippets, feature names, and package/example references in `README.md` and `docs/official_docs/`) are validated in CI by `scripts/check-doc-examples.sh` against `cargo metadata`. The PR tier compiles the standalone example workspaces and cargo-adk templates, so if you change a public API, keep those in sync.
- Update the crate's `README.md` if capabilities changed.
- Keep `AGENTS.md` under 60 KB — agent harnesses truncate larger instruction files. Detail about a
  crate belongs in its README, and about a feature in `adk-rust/Cargo.toml`.
- Add a changelog fragment under `changelog.d/` for user-facing changes (`<section>.<slug>.md`,
  see `changelog.d/README.md`). `CHANGELOG.md` itself changes only in release commits.

## Adding new code

### New LLM provider

1. Implement `adk_core::Llm` in `adk-model/src/<provider>/`
2. Add feature flag to `adk-model/Cargo.toml`
3. Re-export from `adk-model/src/lib.rs` behind the feature
4. Implement `schema_adapter()` if the provider has specific schema requirements
5. Add example in [adk-playground](https://github.com/zavora-ai/adk-playground) or as a standalone crate in `examples/`

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
├── Cargo.lock          # committed; the examples gate checks with --locked
├── src/main.rs         # Entry point with dotenvy, tracing, banner
├── README.md           # Documentation
└── .env.example        # Required env vars
```

## Writing style

This guideline governs every text artefact an agent commits or posts: commit messages, code
comments, rustdoc, PR bodies, issue comments, and review replies. `CONTRIBUTING.md` and
`docs/official_docs/` are the reference corpus — match their register.

The register rules below govern text committed to or posted on the repository. They do not govern
`AGENTS.md` itself, which is instruction addressed to the agent and uses the second-person
imperative.

Committed and posted text is project documentation, not a contributor's voice. Do not adopt a
maintainer's tone, do not offer personal preferences or recommendations in committed artefacts,
and keep analysis, discovery narrative, and reasoning in the conversation with the human rather
than in the repository.

The house register:

- **Structure over prose**: enumerations become tables, procedures become numbered headed steps,
  definitions become bold-lead bullets.
- **Present tense, stating what the system does**: "The gate fails the build when a feature-gated
  module stops compiling."
- **"You" for the reader, "we" only for project-level decisions**: "We follow an issue-first
  workflow."
- **Rationale is a single clause, not a paragraph**: "Excluded so `--all-features` works without
  CUDA toolkit."
- **Code blocks are complete**, with full imports, not fragments.
- **Callouts** are `> **Note:**` blockquotes or inline `**Important:**`.
- **Em dashes are house style** — use them where they read naturally.

### Commit messages

- Conventional-commit subject, imperative, under ~70 characters:
  `fix(rag): upgrade lancedb to 0.31, unbreak and cover the backend`. More examples:

  ```
  feat(gemini): add Vertex AI streaming backend
  fix(realtime): correct audio delta byte handling
  docs: update CONTRIBUTING.md
  test(eval): add trajectory property tests
  ```

- Body only when the change is not self-evident from the subject. When present, keep it short and
  factual, prefer bullets over paragraphs, and state what changed and why in as few lines as
  possible.
- Reference issues as `Fixes #123` or `Closes #123`.
- No discovery narrative, no first person, no essay-length bodies.

### Code comments

- Explain **why**, not **what** — the code already states what it does.
- Single line where possible. A multi-line block earns its length by recording a non-obvious
  constraint, invariant, or upstream quirk.
- Do not restate the code. Do not write changelog entries or migration history in comments.

```rust
// All arrow types come from lancedb's own re-exports, so the arrow version is
// stated exactly once, by lancedb, and cannot drift between crates.
use lancedb::arrow::arrow_schema::{DataType, Field, Schema};
```

### Rustdoc

Follow the pattern in `CONTRIBUTING.md` ("Documentation"): brief one-line summary, more detail if
needed, `# Example` where practical, `# Errors` when the function returns `Result`. Present tense,
third person, no "we" or "I".

```rust
/// Swaps the active LoRA adapter without reloading the base model.
///
/// # Errors
///
/// Returns `MistralRsError::AdapterNotFound` when `adapter_name` is not among the loaded adapters.
pub async fn swap_adapter(&self, adapter_name: &str) -> Result<()> { ... }
```

### PR bodies, issue comments, and review replies

- Lead with what changed, then use the PR template's headed sections (`## What`, `## Why`,
  `## How`) and complete its checklist.
- Prefer tables and bullets over paragraphs.
- State verification results as facts: "`cargo nextest run --workspace` passes; three regression
  tests added in `adk-rag/tests/`."
- No narration of the authoring process, no opinions framed as personal preference, no persuasion.

### Do and don't

| Don't | Do |
|-------|----|
| "Root cause was lancedb sitting four breaking releases stale, not arrow drifting up." | "lancedb 0.28 and later re-export every arrow crate they depend on." |
| "I verified the workspace compiles and my recommendation is the PR tier." | "`cargo check --workspace` passes. `feature-coverage` runs in the PR tier." |
| "This gate fails loudly, which is strictly better than the negligent silence it replaced." | "The gate fails the build when a feature-gated module stops compiling." |
| "Worth noting, one sequencing consequence is that two things are left." | A `### Remaining work` heading followed by a list. |
| A 20-line narrative commit body recounting the investigation. | `fix(realtime): drop session guard before provider await` |
| "I'd put the arrow pin in adk-rag since that's where it hurts most." | "`adk-rag` takes arrow types from `lancedb::arrow`, so the version is stated once, by lancedb." |
| A paragraph listing four authorization mechanisms and their tradeoffs. | A `\| Mechanism \| Use Case \| Granularity \| Runtime \|` table. |

## PR workflow

- **Branch naming**: Use `prefix/short-description`. Allowed prefixes: `feat/`, `fix/`, `docs/`, `refactor/`, `test/`, `chore/`.
- **Reference**: Include `Fixes #123` or similar in the PR description.
- **Scope**: Keep PRs focused — one logical change per PR. Don't mix unrelated changes.
- **Local gates before pushing**: the [quality gates](#quality-gates). `scripts/validate-pr.sh` runs the PR tier scoped to the branch's changes (see CONTRIBUTING.md, "Validating a Branch Locally").
- **CI gates that block merge**: `pr-gate` (all `ci.yml` PR-tier jobs) and `semver`; see [CI cost tiers](#ci-cost-tiers). CONTRIBUTING.md ("Branch Protection — Required Status Checks") is authoritative.

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
   - Changelog fragment added under `changelog.d/` for user-facing changes.
   - `README.md` updated if crate capabilities changed.
   - Examples added or updated for new features.

## Dependency changes

- If you change `Cargo.toml` or `Cargo.lock`, run `cargo check --workspace` to verify resolution.
- Internal crate dependencies use workspace inheritance: `adk-core = { workspace = true }` in member crates.
- The root `Cargo.toml` `[workspace.dependencies]` section pins all internal crate versions.

## Publishing to crates.io

Always verify builds during publish — never use `--no-verify`. Verification ensures the packaged crate compiles correctly for consumers.

### Publish order

Crates publish in dependency order, which `cargo xtask publish` (via `./publish.sh`) computes from
`cargo metadata` at run time; `cargo xtask publish --resume --dry-run` prints it.
`scripts/check-publish-order.sh` is the gate that keeps it satisfiable, and `adk-rust` always
publishes last.

> **Note:** internal **dev**-dependencies are declared path-only (no version) so
> they do not constrain this order. Adding a versioned internal dev-dependency can
> deadlock the release; `check-publish-order.sh` warns when one appears.

### Publish workflow

1. Ensure all quality gates pass and the version is bumped in `[workspace.package]` and all `[workspace.dependencies]` entries.
2. Tag the release: `git tag v<version> && git push origin v<version>`.
3. Publish one crate at a time with verification:

```bash
cargo publish -p <crate-name>
```

4. Cargo waits for crates.io indexing automatically after upload. If a dependent crate fails with "failed to select a version", wait a minute and retry.

### Version checklist

- [ ] `[workspace.package] version` in root `Cargo.toml`
- [ ] All `adk-*` entries in `[workspace.dependencies]`
- [ ] Changelog fragments assembled into `CHANGELOG.md` (`bash scripts/changelog-assemble.sh --into <version>`)
- [ ] Git tag created and pushed
- [ ] GitHub release created with release notes

## Crate-specific guides

- `adk-gemini/AGENTS.md` — Gemini client tracing conventions and instrumentation patterns
- `STABILITY.md` — Crate stability tiers (Stable/Beta/Experimental), deprecation policy, 1.0 milestone
