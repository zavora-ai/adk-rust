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

CI enforces three gates: format, lint, and test. The easiest way to catch failures before they reach CI is to let [lefthook](https://github.com/evilmartians/lefthook) run them automatically as git hooks (see [Git Hooks with Lefthook](#git-hooks-with-lefthook)):

- **pre-commit** runs `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets -- -D warnings`, plus `shellcheck --severity=warning` on any staged shell scripts
- **pre-push** runs `cargo check --workspace` (a fast compilation check — CI runs the full test suite)

Once installed, these run on every `git commit` and `git push` — no extra steps required.

Alternatively, run the gates manually before every commit:

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

- Rust 1.94.0+ (edition 2024)
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

### Git Hooks with Lefthook

The repo ships a `lefthook.yml` that wires the [Quality Gates](#quality-gates) into git hooks, so they run automatically:

- **pre-commit** — format check (`cargo fmt --all -- --check`), lint (`cargo clippy --workspace --all-targets -- -D warnings`), and shell-script lint (`shellcheck --severity=warning` on staged `*.sh`, mirroring the shellcheck hook in `devenv.nix`)
- **pre-push** — a fast compilation check (`cargo check --workspace`), not the full test suite. CI is the full-suite safety net, so the local push gate stays quick (see [Quality Gates](#quality-gates) for the CI tier design)

The shellcheck gate only runs when shell scripts are staged, and runs at `--severity=warning` so informational notes (e.g. SC1091 about sourced files that can't be followed) don't block commits. `publish.sh` is skipped because it's a zsh script, which shellcheck doesn't support. Install shellcheck with `brew install shellcheck` (macOS) or `apt install shellcheck` (Debian/Ubuntu); `scripts/setup-dev.sh` installs it for you.

**devenv users: nothing to do.** The dev shell ships `lefthook` and `shellcheck` and registers the hooks automatically on shell entry — lefthook is the single hook manager for this repo (devenv's own `git-hooks` integration is not used, so the two never fight over `.git/hooks`).

Everyone else, install [lefthook](https://github.com/evilmartians/lefthook):

```bash
brew install lefthook        # macOS
# or: cargo install lefthook  # any platform
# or: npm install -g lefthook # via npm
```

Then register the hooks in your local clone (run once, from the repo root):

```bash
lefthook install
```

From now on, commits run fmt + clippy (and shellcheck on staged shell scripts) and pushes run a `cargo check --workspace` compilation check. If a gate fails, the commit or push is blocked with guidance on how to fix it.

Need to bypass a hook in an emergency? Use `LEFTHOOK=0 git commit ...` or `git commit --no-verify` — but CI will still enforce the gates, so prefer fixing the issue.

## Project Structure

ADK-Rust is a Cargo workspace with 32 publishable crates organized by responsibility.

### Core Crates (publishable to crates.io)

```
adk-core/          Core traits: Agent, Tool, Llm, Session, Event, Content, State
adk-rust-macros/   Procedural macros (#[tool] attribute)
adk-agent/         Agent implementations: LlmAgent, SequentialAgent, ParallelAgent,
                   LoopAgent, ConditionalAgent, LlmConditionalAgent
adk-model/         LLM provider facade: Gemini, OpenAI, Anthropic, DeepSeek, Groq, Ollama,
                   OpenRouter, Bedrock, Azure AI, and OpenAI-compatible presets
adk-gemini/        Dedicated Gemini client with GeminiBackend trait (Studio + Vertex AI)
adk-anthropic/     Dedicated Anthropic client with thinking, caching, citations, vision
adk-tool/          Tool utilities, built-in tools, MCP integration (rmcp 1.3)
adk-runner/        Agent execution runtime with event streaming
adk-server/        REST API server and A2A (Agent-to-Agent) protocol
adk-session/       Session management and state persistence, encrypted sessions
adk-artifact/      Artifact storage (files, images, structured data)
adk-memory/        Long-term memory and RAG integration
adk-graph/         Graph-based workflow orchestration with checkpoints, durable resume, HITL
adk-realtime/      Real-time bidirectional audio/voice agents (OpenAI, Gemini Live, LiveKit)
adk-browser/       Browser automation tools via WebDriver
adk-eval/          Agent evaluation framework (trajectory, semantic, rubric, LLM-judge)
adk-telemetry/     OpenTelemetry 0.31 integration for agent observability
adk-guardrail/     Input/output guardrails for agent safety
adk-auth/          Authentication: API keys, JWT, OAuth2, OIDC, SSO
adk-plugin/        Plugin system for agent lifecycle hooks
adk-skill/         Skill discovery and convention-based agent capabilities
adk-cli/           Command-line launcher for agents
adk-code/          Code generation and execution (experimental)
adk-sandbox/       Sandboxed execution environments (experimental)
adk-audio/         Audio processing, STT/TTS providers (experimental)
adk-rag/           Retrieval-augmented generation pipelines
adk-action/        Action node execution for deterministic workflow operations
adk-deploy/        Deployment utilities
adk-payments/      Payment integration for agent services
cargo-adk/         Cargo subcommand for project scaffolding
adk-rust/          Umbrella crate re-exporting all of the above
```

### Excluded from Workspace

```
adk-mistralrs/     Local LLM inference via mistral.rs (GPU deps — build explicitly)
                   Excluded so `--all-features` works without CUDA toolkit.
                   Has its own CI workflow.
adk-studio/        Visual agent builder — extracted to standalone repo.
                   Repo: https://github.com/zavora-ai/adk-studio
adk-ui/            Dynamic UI generation — extracted to standalone repo.
                   Repo: https://github.com/zavora-ai/adk-ui
```

### Examples

```
examples/          Standalone example crates for integration validation.
                   120+ additional examples in the adk-playground repo:
                   https://github.com/zavora-ai/adk-playground
```

Complex examples are standalone crates added to the root `[workspace.members]`.

### Documentation

```
docs/official_docs/  Comprehensive documentation site content. Cargo snippets,
                     feature names, and package/example references in these pages
                     are validated against `cargo metadata` by
                     scripts/check-doc-examples.sh.
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

The simplest option is to install the git hooks (see [Git Hooks with Lefthook](#git-hooks-with-lefthook)) and let them run the gates on commit and push. To validate manually, run all three in sequence:

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

### CI Cost Tiers

CI is organized into cost tiers so per-PR feedback stays fast while full coverage is
preserved (see the `ci-pipeline-restructure` spec). No coverage is dropped —
expensive axes just move to a later tier.

| Tier | Trigger | Checks | Blocks merge? |
|------|---------|--------|---------------|
| **PR** | pull request (`ci.yml` + `semver.yml`) | `fmt` (prerequisite gate), `clippy --workspace -D warnings`, `nextest --workspace` on Linux (at most once), `feature-coverage` (feature-gated modules like `adk-agent --features codeact`), `docs` (single `cargo doc --workspace --no-deps`), `templates`, compile-only `macos`/`windows` builds, `semver` (stable strict, beta warn-only) | Yes — this is the required-check set |
| **Merge** | `push: main` (`ci-merge.yml`) | cross-platform `nextest --workspace` on macOS/Windows, out-of-workspace Monty build, doc-example compilation | No — runs post-merge |
| **Nightly** | `schedule` (`ci-nightly.yml`) | feature-combination matrix, `cargo-audit`/`cargo-deny` supply-chain, `#[ignore]` integration tests gated on secrets | No — runs on a schedule |

Only the **PR tier** gates merges. The merge and nightly tiers run after a change
lands (or on a schedule) and are informational. The three local gates above map to
the PR tier; locally, `pre-push` runs the lighter `cargo check --workspace` because
CI is the full-suite safety net. The authoritative required-check set is enumerated
in [Branch Protection — Required Status Checks](#branch-protection--required-status-checks).

## Branch Protection — Required Status Checks

CI is organized into cost tiers (see the `ci-pipeline-restructure` spec). Only the
**PR tier** (Tier 1) gates merges. The merge tier and nightly tier run *after* a
change lands (or on a schedule) and are informational — they MUST NOT be marked
branch-protection-required, because requiring a check that doesn't run on a PR
would block every PR waiting for a status that never arrives.

Branch protection for `main` is configured through the GitHub API / repo settings
(there is no committed config file). The authoritative required-check set is the
list below. When updating branch protection, use exactly these status-check
contexts.

### Required on PRs (branch-protection-required)

These are the Tier-1 jobs from `ci.yml` and `semver.yml`. The status-check context
name is the job name (matrix jobs include the matrix value in parentheses):

| Check context | Workflow | Job |
|---------------|----------|-----|
| `fmt` | `ci.yml` | `fmt` |
| `clippy` | `ci.yml` | `clippy` |
| `test` | `ci.yml` | `test` |
| `feature-coverage (adk-agent, codeact)` | `ci.yml` | `feature-coverage` (matrix) |
| `docs` | `ci.yml` | `docs` |
| `templates` | `ci.yml` | `templates` |
| `macos` | `ci.yml` | `macos` (compile-only build) |
| `windows` | `ci.yml` | `windows` (compile-only build) |
| `semver` | `semver.yml` | `semver` (stable-tier strict; beta is warn-only within the same job) |

Notes:
- `feature-coverage` is a matrix job; its context includes the matrix value, so
  add each entry that exists. Today the only entry is `adk-agent, codeact`. If you
  append matrix entries, add their contexts here and to branch protection.
- `semver` is a single job that runs the strict stable-crate check (which can fail
  the job) and a warn-only beta/experimental check (which never fails). Requiring
  the `semver` job therefore requires only the stable-tier semver gate, keeping the
  beta check advisory (Requirement 5.3).
- `codeact-feature` (from `codeact-monty.yml`) is **path-filtered** to CodeAct
  paths and does **not** run on most PRs, so it MUST NOT be required — the
  always-on `feature-coverage (adk-agent, codeact)` job is the merge-blocking
  CodeAct signal instead.

### NOT required (informational tiers)

These run post-merge or on a schedule and MUST NOT be branch-protection-required:

- **Merge tier** (`ci-merge.yml`, `on: push: branches:[main]`):
  `cross-platform-test (macos-latest)`, `cross-platform-test (windows-latest)`,
  `out-of-workspace-monty`, `doc-examples`.
- **Nightly tier** (`ci-nightly.yml`, `on: schedule`): the `features (…)`
  feature-combination matrix jobs, `supply-chain`, `integration-tests`.
- **Weekly out-of-workspace** (`codeact-monty.yml` cron): `monty-runtime`; and the
  path-filtered `codeact-feature` PR job (see note above).

### Applying the required-check set

A maintainer with admin rights can apply the set above with the GitHub CLI. This
requires new PR-tier job names to be stable and green first (spec tasks 6 and 10),
since branch protection waits on a context that must actually report:

```bash
gh api -X PUT repos/zavora-ai/adk-rust/branches/main/protection \
  --input - <<'JSON'
{
  "required_status_checks": {
    "strict": true,
    "checks": [
      { "context": "fmt" },
      { "context": "clippy" },
      { "context": "test" },
      { "context": "feature-coverage (adk-agent, codeact)" },
      { "context": "docs" },
      { "context": "templates" },
      { "context": "macos" },
      { "context": "windows" },
      { "context": "semver" }
    ]
  },
  "enforce_admins": false,
  "required_pull_request_reviews": { "required_approving_review_count": 1 },
  "restrictions": null
}
JSON
```

Adjust `required_pull_request_reviews`, `enforce_admins`, and `restrictions` to
match the project's review policy; the merge-blocking contract for this spec is the
`checks` list above.

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

Key feature flags on `adk-model`:

- `gemini` (default) — Gemini provider
- `openai` — OpenAI provider
- `anthropic` — Anthropic provider
- `deepseek` — DeepSeek provider
- `ollama` — Ollama local models
- `groq` — Groq provider
- `openrouter` — OpenRouter provider
- `bedrock` — Amazon Bedrock
- `azure-ai` — Azure AI Inference

Key feature presets on `adk-rust` (umbrella crate):

- `standard` (default) — agents, models, tools, sessions, runner, server, CLI, telemetry, auth
- `full` — standard + graph, realtime, browser, eval, rag
- `labs` — standard + experimental crates (code, sandbox, audio)
- `minimal` — agents + Gemini + runner (fastest build)

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
- Doc examples: Cargo snippets, feature names, and package/example references in `README.md` and `docs/official_docs/` are validated against `cargo metadata` by `scripts/check-doc-examples.sh`. Compile coverage comes from the workspace example crates and cargo-adk templates.

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

- Edition 2024, MSRV 1.94.0
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
4. Add an example in [adk-playground](https://github.com/zavora-ai/adk-playground)
5. Update `adk-studio` codegen if the provider should be available in Studio (separate repo: `../adk-studio/`)

### Adding a New Tool

1. Implement `adk_core::Tool` trait
2. Add to `adk-tool/src/` or create a new crate if it has heavy deps
3. Write tests (unit + property if applicable)
4. Add an example demonstrating usage

### Adding a New Example

Add examples to the [adk-playground](https://github.com/zavora-ai/adk-playground) repo. For integration validation examples that need to live in this workspace, create a standalone crate:

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

`adk-mistralrs` is published to crates.io as a workspace member. GPU features (`cuda`, `metal`) are opt-in. It has its own CI workflow for GPU-specific testing. Build it with:

```bash
cargo build -p adk-mistralrs
cargo build -p adk-mistralrs --features metal  # macOS GPU
cargo build -p adk-mistralrs --features cuda   # NVIDIA GPU
```

## Getting Help

- [GitHub Issues](https://github.com/zavora-ai/adk-rust/issues) — bug reports and feature requests
- [GitHub Discussions](https://github.com/zavora-ai/adk-rust/discussions) — questions and ideas

## License

By contributing, you agree that your contributions will be licensed under the [Apache 2.0 License](LICENSE).
