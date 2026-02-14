# Contributing to ADK-Rust

Thank you for your interest in contributing to ADK-Rust! This document provides guidelines and instructions for contributing to the Rust Agent Development Kit.

## Table of Contents

- [Getting Started](#getting-started)
- [Project Structure](#project-structure)
- [Development Workflow](#development-workflow)
- [Build Commands](#build-commands)
- [Testing](#testing)
- [Code Style](#code-style)
- [Pull Request Process](#pull-request-process)
- [Architecture Notes](#architecture-notes)

## Getting Started

```bash
# Clone and build
git clone https://github.com/zavora-ai/adk-rust.git
cd adk-rust
cargo build --workspace

# Run all tests
cargo test --workspace

# Check lints (strict — warnings are errors in CI)
cargo clippy --workspace --all-targets -- -D warnings

# Format
cargo fmt --all
```

### Prerequisites

- Rust 1.85.0+ (edition 2024)
- For browser examples: Chrome/Chromium
- For mistral.rs: see [adk-mistralrs section](#adk-mistralrs)

### Environment Setup

Copy `.env.example` to `.env` and fill in API keys for the providers you want to test:

```bash
cp .env.example .env
# Edit .env with your keys (GOOGLE_API_KEY, OPENAI_API_KEY, etc.)
```

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
                   Build: cargo build --manifest-path adk-mistralrs/Cargo.toml
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

## Development Workflow

### Branch Strategy

- `main` — stable, protected (PRs required, CI must pass)
- Feature branches: `feat/<description>`, `fix/<description>`, `docs/<description>`

### Typical Flow

1. Create a feature branch from `main`
2. Make changes, ensure `cargo clippy --workspace --all-targets -- -D warnings` is clean
3. Run `cargo test --workspace` and `cargo fmt --all`
4. Push and open a PR against `main`

## Build Commands

The `Makefile` provides common shortcuts:

| Command | Description |
|---------|-------------|
| `make build` | Build all workspace crates |
| `make build-all` | Build with all features |
| `make test` | Run all workspace tests |
| `make clippy` | Run clippy lints |
| `make fmt` | Format all code |
| `make examples` | Build all examples (CPU-only) |
| `make docs` | Generate and open rustdoc |
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
- `full` — All realtime providers

## Testing

```bash
# Full workspace
cargo test --workspace

# Single crate
cargo test -p adk-core
cargo test -p adk-gemini

# With features
cargo test -p adk-realtime --features full

# Standalone crate (Ralph)
cargo test -p ralph
```

### Test Organization

- Unit tests: `#[cfg(test)]` modules in source files
- Integration tests: `tests/*.rs` in each crate
- Property tests: `tests/*_property_tests.rs` using `proptest` (100+ iterations)
- Doc examples: validated via `docs/official_docs_examples/` workspace members

### CI Gate

CI runs:

1. `cargo fmt --all -- --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace`

All three must pass for PR merge.

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

## Pull Request Process

### Before Submitting

- [ ] `cargo fmt --all` — code is formatted
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` — zero warnings
- [ ] `cargo test --workspace` — all tests pass
- [ ] Documentation updated for any API changes
- [ ] New public APIs have rustdoc with examples
- [ ] CHANGELOG.md updated if user-facing

### Commit Messages

Use conventional commits:

```
feat(gemini): add Vertex AI streaming backend
fix(realtime): correct audio delta byte handling
docs: update CONTRIBUTING.md with full crate list
refactor(ui): extract validation into per-type trait impls
test(eval): add trajectory property tests
```

### PR Guidelines

- Keep PRs focused — one logical change per PR
- Don't mix unrelated changes (formatting, refactoring, features)
- Reference issue numbers where applicable (`Fixes #77`)
- Include a clear description of what changed and why

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
