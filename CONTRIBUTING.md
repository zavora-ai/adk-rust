# Contributing to ADK-Rust

Thank you for your interest in contributing to ADK-Rust! This document provides guidelines for contributing to the Rust Agent Development Kit.

## Table of Contents

- [Contribution Workflow](#contribution-workflow)
- [Getting Started](#getting-started)
- [Project Structure](#project-structure)
- [Quality Gates](#quality-gates)
- [Build Commands](#build-commands)
- [Testing](#testing)
- [Code Style](#code-style)
- [Pull Request Checklist](#pull-request-checklist)
- [Architecture Notes](#architecture-notes)

## Contribution Workflow

We follow an issue-first workflow. Every code change should trace back to an issue for visibility and coordination.

### 1. Open or Claim an Issue

Before writing code, make sure there's a GitHub issue for the work:

- **Bug?** Open a [bug report](https://github.com/zavora-ai/adk-rust/issues/new?template=bug_report.md) with reproduction steps.
- **Feature?** Open a [feature request](https://github.com/zavora-ai/adk-rust/issues/new?template=feature_request.md) describing the motivation and proposed approach.
- **Already exists?** Comment on the issue to signal you're working on it.

This gives everyone visibility into work in progress and avoids duplicate effort.

### 2. Create a Feature Branch

```bash
git checkout main
git pull origin main
git checkout -b feat/my-feature    # or fix/my-bug, docs/my-update
```

Branch naming conventions:
- `feat/<description>` — new features
- `fix/<description>` — bug fixes
- `docs/<description>` — documentation changes
- `refactor/<description>` — code improvements without behavior change
- `test/<description>` — test additions or improvements

### 3. Develop with Quality Gates

Run these before every commit. CI enforces them — save yourself the round-trip:

```bash
cargo fmt --all                                          # Format
cargo clippy --workspace --all-targets -- -D warnings    # Lint (zero warnings)
cargo nextest run --workspace                            # Test (parallel, ~10x faster)
```

### 4. Submit a PR

- Reference the issue: `Fixes #123` in the PR description
- Fill out the [PR template checklist](#pull-request-checklist)
- Keep PRs focused — one logical change per PR
- Don't mix unrelated changes (formatting fixes, refactoring, other features)

### 5. Review and Merge

- PRs require review and passing CI before merge
- Address review feedback with additional commits (don't force-push during review)
- Squash or merge commit at maintainer discretion

## Getting Started

```bash
# Clone and build
git clone https://github.com/zavora-ai/adk-rust.git
cd adk-rust

# Option A: Nix/devenv (reproducible, recommended for contributors)
devenv shell

# Option B: Install tools manually
make setup          # Installs sccache, cmake, etc. for your platform
# Or just check what you have:
make check-env

# Build and validate
cargo build --workspace
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
```

### Prerequisites

- Rust 1.85.0+ (edition 2024)
- For browser examples: Chrome/Chromium
- For `openai-webrtc` feature: cmake (audiopus builds Opus from source)
- For mistral.rs: see [adk-mistralrs section](#adk-mistralrs)

### Dev Environment Setup

We provide three ways to set up your development environment:

**Option A: devenv.nix (Recommended for reproducibility)**

If you use [devenv](https://devenv.sh), just run `devenv shell`. This gives you identical toolchains on Linux, macOS, and CI — Rust, sccache, mold, cmake, Node.js, and everything else pinned to known-good versions.

**Option B: Setup script (brew/apt)**

```bash
./scripts/setup-dev.sh          # Install recommended tools
./scripts/setup-dev.sh --check  # Just check what's installed
```

**Option C: Manual**

Install [sccache](https://github.com/mozilla/sccache) for compilation caching (cuts rebuild times by ~70%):

```bash
brew install sccache    # macOS
# or: apt install sccache  # Linux
# or: cargo install sccache --locked

# Add to your shell profile (~/.zshrc, ~/.bashrc):
export RUSTC_WRAPPER=sccache
export CMAKE_POLICY_VERSION_MINIMUM=3.5  # needed for cmake 4.x
```

On Linux, install [mold](https://github.com/rui314/mold) for faster linking (`.cargo/config.toml` uses it automatically):

```bash
sudo apt install mold
```

### Environment Variables

Copy `.env.example` to `.env` and fill in API keys for the providers you want to test:

```bash
cp .env.example .env
# Edit .env with your keys (GOOGLE_API_KEY, OPENAI_API_KEY, etc.)
```

**Important:** Never commit `.env` files, API keys, local paths, or IDE configuration.

## Project Structure

ADK-Rust is a Cargo workspace with 25+ crates organized by responsibility.

### Core Crates (publishable to crates.io)

```
adk-core/          Core traits: Agent, Tool, Llm, Session, Event, Content
adk-agent/         Agent implementations: LlmAgent, SequentialAgent, ParallelAgent,
                   LoopAgent, ConditionalAgent, LlmConditionalAgent
adk-model/         LLM provider facade: Gemini, OpenAI, Anthropic, DeepSeek, Groq, Ollama
adk-gemini/        Dedicated Gemini client with GeminiBackend trait (Studio + Vertex AI)
adk-tool/          Tool utilities, built-in tools, MCP integration (rmcp 0.14)
adk-runner/        Agent execution runtime with event streaming
adk-server/        REST API server and A2A (Agent-to-Agent) protocol
adk-session/       Session management and state persistence
adk-artifact/      Artifact storage (files, images, structured data)
adk-memory/        Long-term memory and RAG integration
adk-graph/         Graph-based workflow orchestration with checkpoints and HITL
adk-realtime/      Real-time bidirectional audio/voice agents (OpenAI, Gemini Live)
adk-browser/       Browser automation tools (Playwright-based)
adk-eval/          Agent evaluation framework (trajectory, semantic, rubric, LLM-judge)
adk-ui/            UI protocol for rich agent responses (tables, charts, cards, modals)
adk-telemetry/     OpenTelemetry integration for agent observability
adk-guardrail/     Input/output guardrails for agent safety
adk-auth/          Authentication: API keys, JWT, OAuth2, OIDC, SSO
adk-plugin/        Plugin system for agent lifecycle hooks
adk-skill/         Skill discovery and convention-based agent capabilities
adk-cli/           Command-line launcher for agents
adk-rust/          Umbrella crate re-exporting all of the above
```

### Application Crates

```
adk-studio/        Visual agent builder with Rust code generation
                   ├── src/         Axum backend (SSE, codegen, project management)
                   ├── ui/          React + ReactFlow frontend
                   └── templates/   Starter project templates
adk-doc-audit/     Documentation quality auditor (rustdoc coverage, link checking)
```

### Excluded from Workspace

```
adk-mistralrs/     Local LLM inference via mistral.rs (GPU deps — build explicitly)
                   Excluded so `--all-features` works without CUDA toolkit.
                   Has its own CI workflow.
```

### Examples

```
examples/          120+ working examples organized by provider/feature
examples/ralph/    Standalone autonomous agent crate (own Cargo.toml + tests/)
```

Examples are workspace members. Simple examples live under `examples/<name>/main.rs` with entries in `examples/Cargo.toml`. Complex examples like Ralph are standalone crates added to the root `[workspace.members]`.

### Documentation

```
docs/official_docs/           Comprehensive documentation site content
docs/official_docs_examples/  Compilable code snippets validating every doc page
```

## Quality Gates

Every PR must pass these checks. CI enforces them automatically.

### The Three Gates

| Gate | Command | What It Catches |
|------|---------|-----------------|
| Format | `cargo fmt --all -- --check` | Inconsistent formatting |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | Warnings, dead code, anti-patterns |
| Test | `cargo nextest run --workspace` | Regressions, broken logic |

### Quick Validation

Run all three in sequence:

```bash
cargo fmt --all && \
cargo clippy --workspace --all-targets -- -D warnings && \
cargo nextest run --workspace
```

If all three pass, your PR will pass CI.

### What "Zero Warnings" Means

Clippy runs with `-D warnings` — every warning is a compile error. This includes:
- Unused imports, variables, and dead code
- Missing documentation on public items (in some crates)
- `println!`/`eprintln!` in library code (use `tracing` instead)
- Unnecessary clones, redundant closures, etc.

Fix warnings before pushing. Don't suppress them with `#[allow(...)]` unless there's a documented reason.

## Build Commands

The `Makefile` provides common shortcuts:

| Command | Description |
|---------|-------------|
| `make setup` | Install/check dev tools (sccache, mold, cmake) |
| `make check-env` | Check what's installed without changing anything |
| `make build` | Build all workspace crates |
| `make build-all` | Build with all features |
| `make test` | Run all workspace tests |
| `make clippy` | Run clippy lints |
| `make fmt` | Format all code |
| `make examples` | Build all examples (CPU-only) |
| `make docs` | Generate and open rustdoc |
| `make cache-stats` | Show sccache hit/miss statistics |
| `make cache-clear` | Clear sccache and cargo caches |
| `make build-mistralrs` | Build adk-mistralrs (CPU) |
| `make build-mistralrs-metal` | Build adk-mistralrs with Metal (macOS) |
| `make build-mistralrs-cuda` | Build adk-mistralrs with CUDA |

### Feature Flags

Key feature flags on `adk-model` and `examples`:

- `openai` — OpenAI provider
- `anthropic` — Anthropic provider
- `deepseek` — DeepSeek provider
- `ollama` — Ollama local models
- `groq` — Groq provider
- `browser` — Browser automation
- `guardrails` — Guardrail support
- `sso` — SSO authentication

`adk-realtime` has its own feature flags:

- `openai` — OpenAI Realtime API (WebSocket)
- `gemini` — Gemini Live API (WebSocket)
- `vertex-live` — Vertex AI Live API (OAuth2 via ADC)
- `livekit` — LiveKit WebRTC bridge
- `openai-webrtc` — OpenAI WebRTC transport (requires cmake)
- `full` — All providers except WebRTC (no cmake needed)
- `full-webrtc` — Everything including WebRTC (requires cmake)

## Testing

ADK-Rust uses [cargo-nextest](https://nexte.st/) for parallel test execution (~10x faster than `cargo test`).

```bash
# Install (one-time, or use devenv which includes it)
curl -LsSf https://get.nexte.st/latest/mac | tar zxf - -C ${CARGO_HOME:-~/.cargo}/bin

# Full workspace
cargo nextest run --workspace

# Single crate
cargo nextest run -p adk-core
cargo nextest run -p adk-gemini

# With features
cargo nextest run -p adk-realtime --features full

# Standalone crate (Ralph)
cargo nextest run -p ralph

# Doctests (nextest doesn't run these)
cargo test --workspace --doc
```

### Test Organization

- Unit tests: `#[cfg(test)]` modules in source files
- Integration tests: `tests/*.rs` in each crate
- Property tests: `tests/*_property_tests.rs` using `proptest` (100+ iterations)
- Doc examples: validated via `docs/official_docs_examples/` workspace members

### Writing Tests for New Code

New code should include tests. The type depends on what you're adding:

| Change Type | Expected Tests |
|-------------|---------------|
| New public function | Unit test with happy path + error cases |
| New trait implementation | Integration test exercising the trait contract |
| Bug fix | Regression test that fails without the fix |
| New crate/module | Unit tests + at least one integration test |
| Serialization/config | Property test with `proptest` (100+ iterations) |

If your change is purely internal refactoring with no behavior change, existing tests passing is sufficient.

## Code Style

### Rust Conventions

- Edition 2024, MSRV 1.85.0
- `thiserror` for library error types
- `async-trait` for async trait methods
- `Arc<T>` for shared ownership across async boundaries
- `tokio::sync::RwLock` for async-safe interior mutability
- Builder pattern for complex configuration
- `tracing` for structured logging (never `println!` or `eprintln!` in library code)

### Error Handling

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MyError {
    #[error("Descriptive message: {0}")]
    Variant(String),

    #[error("Actionable guidance: {details}. Try doing X instead.")]
    WithGuidance { details: String },
}
```

### Documentation

Every public item needs rustdoc. Include `# Example` sections where practical:

```rust
/// Brief one-line description.
///
/// More detail if needed.
///
/// # Example
///
/// ```rust,ignore
/// let result = my_function("input")?;
/// ```
pub fn my_function(input: &str) -> Result<Output> { ... }
```

### Commit Messages

Use conventional commits:

```
feat(gemini): add Vertex AI streaming backend
fix(realtime): correct audio delta byte handling
docs: update CONTRIBUTING.md with full crate list
refactor(ui): extract validation into per-type trait impls
test(eval): add trajectory property tests
```

## Pull Request Checklist

Every PR has a template with this checklist. Fill it out when you open your PR.

### Quality Gates (all required)

- [ ] `cargo fmt --all` — code is formatted
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` — zero warnings
- [ ] `cargo nextest run --workspace` — all tests pass
- [ ] Builds clean: `cargo build --workspace`

### Code Quality

- [ ] New code has tests (unit, integration, or property tests as appropriate)
- [ ] Public APIs have rustdoc comments with `# Example` sections
- [ ] No `println!`/`eprintln!` in library code (use `tracing` instead)
- [ ] No hardcoded secrets, API keys, or local paths

### Hygiene

- [ ] No local development artifacts (`.env`, `.DS_Store`, IDE configs, build dirs)
- [ ] No unrelated changes mixed in (formatting, refactoring, other features)
- [ ] Commit messages follow conventional format (`feat:`, `fix:`, `docs:`, etc.)
- [ ] PR targets `main` branch
- [ ] PR references an issue (`Fixes #___`)

### Documentation (if applicable)

- [ ] CHANGELOG.md updated for user-facing changes
- [ ] README updated if crate capabilities changed
- [ ] Examples added or updated for new features

## Architecture Notes

### Adding a New LLM Provider

1. Implement `adk_core::Llm` trait in `adk-model/src/<provider>/`
2. Add feature flag to `adk-model/Cargo.toml`
3. Re-export from `adk-model/src/lib.rs` behind the feature
4. Add an example under `examples/<provider>_basic/`
5. Update `adk-studio` codegen if the provider should be available in Studio

### Adding a New Tool

1. Implement `adk_core::Tool` trait
2. Add to `adk-tool/src/` or create a new crate if it has heavy deps
3. Write tests (unit + property if applicable)
4. Add an example demonstrating usage

### Adding a New Example

Simple (single file):
```
examples/<name>/main.rs
# Add [[example]] entry to examples/Cargo.toml
```

Complex (standalone crate):
```
examples/<name>/
├── Cargo.toml
├── src/
│   ├── lib.rs
│   └── main.rs
├── tests/
└── README.md
# Add to [workspace.members] in root Cargo.toml
```

### adk-mistralrs

This crate depends on `mistral.rs` which uses git dependencies (candle). It cannot be published to crates.io and is excluded from the workspace to avoid requiring CUDA toolkit for `--all-features` builds. It has its own CI workflow. Build it explicitly:

```bash
cargo build --manifest-path adk-mistralrs/Cargo.toml
cargo build --manifest-path adk-mistralrs/Cargo.toml --features metal  # macOS GPU
cargo build --manifest-path adk-mistralrs/Cargo.toml --features cuda   # NVIDIA GPU
```

## Getting Help

- [GitHub Issues](https://github.com/zavora-ai/adk-rust/issues) — bug reports and feature requests
- [GitHub Discussions](https://github.com/zavora-ai/adk-rust/discussions) — questions and ideas

## License

By contributing, you agree that your contributions will be licensed under the [Apache 2.0 License](LICENSE).
