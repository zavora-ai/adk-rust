# Contributing to ADK-Rust

Thank you for your interest in contributing to ADK-Rust! This document provides guidelines for contributing to the Rust Agent Development Kit.

## Table of Contents

- [Contribution Workflow](#contribution-workflow)
- [Getting Started](#getting-started)
- [Project Structure](#project-structure)
- [Quality Gates](#quality-gates)
- [Validating a Branch Locally](#validating-a-branch-locally)
- [Branch Protection — Required Status Checks](#branch-protection--required-status-checks)
- [Build Commands](#build-commands)
- [Testing](#testing)
- [Code Style](#code-style)
- [Pull Request Checklist](#pull-request-checklist)
- [Architecture Notes](#architecture-notes)
- [Release Process](#release-process)
- [Getting Help](#getting-help)

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
- `chore/<description>` — tooling, CI, and maintenance

### 3. Develop with Quality Gates

CI enforces three gates: format, lint, and test. The easiest way to catch failures before they
reach CI is to let [lefthook](https://github.com/evilmartians/lefthook) run the fast checks as git
hooks on every commit and push — see [Git Hooks with Lefthook](#git-hooks-with-lefthook).
[Quality Gates](#quality-gates) says what else to run before you commit.

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

# Option B: Install tools with the setup script (make setup runs the same script)
./scripts/setup-dev.sh
# Or just check what you have:
./scripts/setup-dev.sh --check

# Build
cargo build --workspace
```

Then run the three [quality gates](#quality-gates) once, the full workspace suite included, to
confirm the setup.

### Prerequisites

- Rust 1.95.0+ (edition 2024), pinned by `rust-toolchain.toml`
- `cargo-nextest` — backs the test gate: `cargo install cargo-nextest --locked`
- For `adk-rag --features lancedb`: `protoc` (lance's build script requires it)
- For `openai-webrtc` feature: cmake (audiopus builds Opus from source)
- For browser examples: Chrome/Chromium
- On Linux outside the devenv shell: `clang` and the `wild` linker (see
  [Dev Environment Setup](#dev-environment-setup))
- For mistral.rs: see [adk-mistralrs section](#adk-mistralrs)
- On Windows: Visual Studio Build Tools (C/C++), NASM (`aws-lc-sys` needs it on
  MSVC), plus full Git for Windows and Python — nine tests shell out to `bash`,
  `sh`, or `python3`

### Dev Environment Setup

We provide three ways to set up your development environment:

**Option A: devenv.nix (Recommended for reproducibility)**

If you use [devenv](https://devenv.sh), just run `devenv shell`. This gives you identical toolchains on Linux, macOS, and CI — Rust, sccache, the `wild` linker, cmake, protobuf, Node.js, and everything else pinned to known-good versions.

**Option B: Setup script**

On Linux and macOS (uses brew/apt/dnf):

```bash
./scripts/setup-dev.sh          # Install recommended tools
./scripts/setup-dev.sh --check  # Just check what's installed
```

On Windows (devenv and the bash script both need a POSIX shell):

```powershell
./scripts/setup-dev.ps1         # Install recommended tools
./scripts/setup-dev.ps1 -Check  # Just check what's installed
```

Both scripts are idempotent, and both check the feature-gated build tools —
`protoc` for `adk-rag --features lancedb`, and NASM for `aws-lc-sys` on Windows
MSVC. Missing either one fails the build locally while CI stays green, because
CI installs them on every runner.

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

On Linux, `.cargo/config.toml` links through `clang` with the `wild` linker, which the devenv
shell provides. Outside that shell, install `clang` and `wild`, or remove the three Linux
`[target.*]` linker sections from your local `.cargo/config.toml` — the shared
`.github/actions/setup-rust` action (`neutralize-wild`) is the reference for which ones. macOS
uses the default linker, and Windows MSVC uses `rust-lld`, which ships with rustup.

### Environment Variables

Copy `.env.example` to `.env` and fill in API keys for the providers you want to test:

```bash
cp .env.example .env
# Edit .env with your keys (GOOGLE_API_KEY, OPENAI_API_KEY, etc.)
```

**Important:** Never commit `.env` files, API keys, local paths, or IDE configuration.

### Git Hooks with Lefthook

The repo ships a `lefthook.yml` that wires the [Quality Gates](#quality-gates) into git hooks, so they run automatically:

- **pre-commit** — format check (`cargo fmt --all -- --check`), lint (`cargo clippy --workspace --all-targets -- -D warnings`), the feature-surface gate (`scripts/check-feature-growth.sh`: a new `adk-rust` feature must change what links or carry a recorded decision — see [docs/feature-surface.md](docs/feature-surface.md)), shell-script lint (`shellcheck --severity=warning` on staged `*.sh`, mirroring the shellcheck hook in `devenv.nix`), and PSScriptAnalyzer on staged `*.ps1` (`scripts/lint-powershell.ps1`, skipped when the module is not installed)
- **pre-push** — a fast compilation check (`cargo check --workspace`), not the full test suite, plus `scripts/check-examples-compile.sh` over the four example shards when an example or a crate it depends on changed. CI is the full-suite safety net, so the local push gate stays quick (see [Quality Gates](#quality-gates) for the CI tier design)

Each gate runs only when a file it checks is staged, so a Markdown-only commit runs none of them. The shellcheck gate runs at `--severity=warning` so informational notes (e.g. SC1091 about sourced files that can't be followed) don't block commits. Install shellcheck with `brew install shellcheck` (macOS) or `apt install shellcheck` (Debian/Ubuntu); `scripts/setup-dev.sh` installs it for you.

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

From now on, commits and pushes run the gates above. If a gate fails, the commit or push is blocked with guidance on how to fix it.

Need to bypass a hook in an emergency? Use `LEFTHOOK=0 git commit ...` or `git commit --no-verify` — but CI will still enforce the gates, so prefer fixing the issue.

## Project Structure

ADK-Rust is a Cargo workspace of publishable crates organized by responsibility. The
[Crates table in the README](README.md#crates) describes each one, and
[STABILITY.md](STABILITY.md) gives its stability tier.

### Excluded from Workspace

```
adk-studio/        Retired in favour of adk-surface's workflow observer
                   (docs/official_docs/surfaces/workflow.md).
                   Repo: https://github.com/zavora-ai/adk-studio
adk-ui/            Retired, for ADK-Rust 2 only: agent-drawn UI is adk-surface's
                   `genui` feature (docs/official_docs/surfaces/genui.md).
                   Repo: https://github.com/zavora-ai/adk-ui
```

### Examples

```
examples/          Standalone example crates for integration validation, each with its
                   own `[workspace]` key and Cargo.lock. More examples live in the
                   adk-playground repo: https://github.com/zavora-ai/adk-playground
```

The root workspace excludes the examples, so `cargo check --workspace` does not compile them;
`scripts/check-examples-compile.sh` does, in the PR tier and on `pre-push`.

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

The git hooks (see [Git Hooks with Lefthook](#git-hooks-with-lefthook)) run the format and lint
gates on every commit and a compilation check on every push. Before each commit, also run the
tests of each crate you changed:

```bash
cargo nextest run -p <crate>
```

The PR tier runs the full workspace suite on every pull request, together with feature coverage,
the PostgreSQL suites, the standalone examples, the scripted scenarios, and the semver check.
[`scripts/validate-pr.sh`](#validating-a-branch-locally) runs those checks on your changes before
you push.

### What "Zero Warnings" Means

Clippy runs with `-D warnings` — every warning is a compile error. This includes:
- Unused imports, variables, and dead code
- Missing documentation on public items (in some crates)
- `println!`/`eprintln!` in library code (use `tracing` instead)
- Unnecessary clones, redundant closures, etc.

Fix warnings before pushing. Don't suppress them with `#[allow(...)]` unless there's a documented reason.

### CI Cost Tiers

CI is organized into cost tiers so per-PR feedback stays fast while full coverage is
preserved. No coverage is dropped —
expensive axes just move to a later tier.

| Tier | Trigger | Checks | Blocks merge? |
|------|---------|--------|---------------|
| **PR** | pull request (`ci.yml` + `semver.yml`) | `fmt`, `web` (the web home's build and generated assets), `clippy --workspace -D warnings`, `nextest --workspace` on Linux, `feature-coverage` (feature-gated modules like `adk-agent --features codeact`), `postgres` (the `#[ignore]`d PostgreSQL suites against a pgvector server, including TLS), docs and doctests, standalone examples (4 shards; `examples/harness_kernel` under `clippy -D warnings`), the scripted scenarios of `examples/harness_kernel` on Linux and of `examples/website`, `templates`, a compile-only macOS build, a Windows build with the code only Windows has (restricted-token enforcer, process trees, owner-only files, the kernel's named pipe and service), and `semver` (stable strict against `SEMVER_RELEASE_TYPE` in `semver.yml`: `minor` fails breakage; `major`, during a major release candidate, accepts every change, so the job reports without building, and `breaking` changelog fragments record the changes; beta warn-only) | Yes — aggregated by `pr-gate`, plus `semver` |
| **Merge** | `push: main` (`ci-merge.yml`, `ci.yml`'s `clippy` and `test`, `sandbox-linux.yml`), and on demand (`gh workflow run ci-merge.yml --ref <branch>`) | cross-platform `nextest --workspace` on macOS/Windows (the Windows kernel scenarios included), the scripted scenarios of `examples/harness_kernel` on macOS, the documented-example check, the computer-use receipt, Linux clippy and tests (which save the build caches pull requests restore); bubblewrap enforcement proven on `ubuntu-24.04` (`sandbox-linux.yml`, also on a pull request that touches the sandbox or the harness shell, and on demand) | No — runs post-merge |
| **Nightly** | `schedule` (`ci-nightly.yml`) | feature-combination matrix, `cargo-audit`/`cargo-deny` supply-chain, `#[ignore]` integration tests gated on secrets | No — runs on a schedule |

Only the **PR tier** gates merges. The merge and nightly tiers run after a change
lands (or on a schedule) and are informational. The three local gates above map to
the PR tier; locally, `pre-push` runs the lighter `cargo check --workspace` because
CI is the full-suite safety net. The authoritative required-check set is enumerated
in [Branch Protection — Required Status Checks](#branch-protection--required-status-checks).

## Validating a Branch Locally

`scripts/validate-pr.sh` runs the PR tier on the current branch, scoped to what it
changes against a base (default `origin/main`): `fmt` on the workspace; clippy and
nextest on the changed crates and every crate that depends on them; the
feature-coverage pairs of the changed crates; each changed crate's widest declared
feature set (`all-providers` or `full`), since a crate such as `adk-model` compiles
almost nothing under its defaults; the semver gate for changed Stable crates when
`cargo-semver-checks` is installed; shellcheck and YAML parsing for
changed scripts and workflows; and the documentation gates. `--examples` adds a
`cargo check --locked` of every standalone example that depends on a changed crate.

```bash
scripts/validate-pr.sh                 # against origin/main
scripts/validate-pr.sh main --examples # against a local ref, with the examples
```

Unless you set `CARGO_TARGET_DIR`, the script builds in the main checkout's `target/`, so running it from a second worktree does not cost a second full build on disk.

The script prints one line per gate and exits non-zero when any gate fails. It is the
maintainer's tool for rebasing a contributor branch and confirming it before pushing;
CI remains the verification of record.

## Branch Protection — Required Status Checks

CI is organized into [cost tiers](#ci-cost-tiers). Only the
**PR tier** (Tier 1) gates merges. The merge tier and nightly tier run *after* a
change lands (or on a schedule) and are informational — they MUST NOT be marked
branch-protection-required, because requiring a check that doesn't run on a PR
would block every PR waiting for a status that never arrives.

The `CI / pr-gate` job depends on every job group in `ci.yml` and runs with
`if: always()`. It succeeds only when every dependency succeeds, including all
matrix entries. Matrix jobs remain visible individually for diagnosis, but adding
or removing a matrix entry no longer requires a matching branch-ruleset edit.

### Required on PRs (branch-protection-required)

Require exactly these stable contexts in the `main` ruleset:

| Check context | Workflow | Job |
|---------------|----------|-----|
| `pr-gate` | `ci.yml` | Aggregate of every PR-tier CI job below |
| `semver` | `semver.yml` | `semver` (stable-tier strict; beta is warn-only within the same job) |

> **Note:** `pr-gate` and `semver` are the only contexts the ruleset requires. Shards
> and matrix entries report individually for diagnosis, but none is required by name,
> so the feature-coverage pairs and the example shards can change without a ruleset
> edit.

`pr-gate` covers `fmt`, `web`, `clippy`, Linux workspace tests, the eight
`feature-coverage` shards driven by `scripts/feature-coverage-pairs.txt`, the
`postgres` job driven by `scripts/test-postgres.sh`, docs and doctests, the four
standalone-example shards, `kernel-scenarios`, `website-scenarios`, `templates`, the macOS build, and the Windows build plus
sandbox portability smokes. `semver` stays separate because its
workflow has an independent trigger and keeps stable crates strict while
beta/experimental crates remain advisory.

The `scope` job narrows a run to what the change can affect, and `pr-gate` accepts a
skipped job only when the scope or the event skipped it:

| Run | Jobs that run |
|-----|---------------|
| Pull request that touches code | every job |
| Pull request that touches only `docs/`, Markdown, or `LICENSE*` | `fmt`, `web`, `templates` (the documentation gates) |
| Merge-queue build | every job except `docs`, the standalone-example shards, `kernel-scenarios`, `website-scenarios` and the macOS and Windows builds, which the pull request already passed and a merge cannot break on its own |

### NOT required (informational tiers)

These run post-merge or on a schedule and MUST NOT be branch-protection-required:

- **Merge tier** (`ci-merge.yml`, `on: push: branches:[main]` and `workflow_dispatch`):
  `cross-platform-test (macos-latest)`, `cross-platform-test (windows-latest)`,
  `kernel-scenarios-macos` (the scripted scenarios of `examples/harness_kernel`),
  `doc-examples`; `sandbox-linux.yml` (`push: main`, path-filtered pull requests,
  `workflow_dispatch`): `bubblewrap`.
- **Nightly tier** (`ci-nightly.yml`, `on: schedule`): the `features (…)`
  feature-combination matrix jobs, `supply-chain`, `durable-stores`, `integration-tests`,
  `website-wordpress` and `website-soak` (spec 3.5's WordPress fixture and S19 soak).
- **Code scanning** (`codeql.yml`, `on: push: branches:[main]` and weekly): the
  `Analyze (…)` CodeQL jobs. Alerts appear under Security → Code scanning; test
  code and examples are excluded by `.github/codeql/config.yml`.

### Applying the required-check set

A maintainer with admin rights applies this through **Settings → Rules →
Rulesets → `adk-rust-ruleset` → Require status checks to pass**. First merge the
workflow change and confirm that `CI / pr-gate` has reported successfully on a
pull request. Then remove the individual CI/matrix contexts and retain only:

- `pr-gate`
- `semver`

Keep **Require branches to be up to date before merging** enabled. Do not change
the ruleset's review, deletion, or non-fast-forward policies as part of this
status-check migration.

## Build Commands

The `Makefile` provides common shortcuts:

| Command | Description |
|---------|-------------|
| `make setup` | Install or check dev tools (`scripts/setup-dev.sh`) |
| `make check-env` | Check what's installed without changing anything |
| `make build` | Build all workspace crates |
| `make build-all` | Build with all features |
| `make test` | Run the workspace test suite (`cargo nextest run --workspace`) |
| `make clippy` | Run the lint gate (`--all-targets -- -D warnings`) |
| `make fmt` | Format all code |
| `make examples` | Compile every standalone example (`scripts/check-examples-compile.sh`) |
| `make docs` | Generate and open rustdoc |
| `make cache-stats` | Show sccache hit/miss statistics |
| `make cache-clear` | Clear sccache and cargo caches |
| `make build-mistralrs` | Build adk-mistralrs (CPU) |
| `make build-mistralrs-metal` | Build adk-mistralrs with Metal (macOS) |
| `make build-mistralrs-cuda` | Build adk-mistralrs with CUDA |

### Feature Flags

The umbrella crate `adk-rust` has four tiers — `minimal` (the default), `standard`, `enterprise`,
and `full` — and every other feature composes with any of them.
[`adk-rust/Cargo.toml`](adk-rust/Cargo.toml) lists each feature, and each crate's `Cargo.toml`
lists its own, such as `adk-model`'s providers and `adk-realtime`'s transports.

## Testing

ADK-Rust uses [cargo-nextest](https://nexte.st/) for parallel test execution (~10x faster than `cargo test`).

```bash
# Install (one-time; devenv and scripts/setup-dev.sh include it)
cargo install cargo-nextest --locked

# Full workspace
cargo nextest run --workspace

# Single crate
cargo nextest run -p adk-core
cargo nextest run -p adk-gemini

# With features
cargo nextest run -p adk-realtime --features full

# Doctests (nextest doesn't run these)
cargo test --workspace --doc
```

### PostgreSQL Tests

Tests that need a PostgreSQL server are `#[ignore]`d, so the commands above skip them.
`scripts/test-postgres.sh` runs every PostgreSQL suite of `adk-session`, `adk-memory`,
`adk-graph`, `adk-auth`, `adk-rag`, and `adk-managed`, as the PR-tier `postgres` job
does. The `adk-memory` and `adk-rag` suites need the pgvector extension, which the
`pgvector/pgvector:pg17` image provides:

```bash
docker run -d --name adk-postgres -p 5432:5432 -e POSTGRES_PASSWORD=postgres pgvector/pgvector:pg17
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres scripts/test-postgres.sh
```

The suites create and drop their own schemas and databases, so a scratch server is
enough. To run one suite, pass its crate, features, and test target:

```bash
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres \
  cargo nextest run -p adk-session --features postgres --run-ignored only --test session_contract_postgres
```

The TLS suite (`adk-session/tests/postgres_tls_tests.rs`) needs a second server with a
certificate from a private CA. `scripts/postgres-tls-server.sh` starts one in Docker;
the script then runs the TLS suite as well:

```bash
scripts/postgres-tls-server.sh /tmp/adk-pg-tls
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres \
DATABASE_URL_TLS=postgres://postgres:postgres@localhost:5433/postgres \
PG_TLS_ROOT_CERT=/tmp/adk-pg-tls/ca.crt \
  scripts/test-postgres.sh
```

### Test Organization

- Unit tests: `#[cfg(test)]` modules in source files
- Integration tests: `tests/*.rs` in each crate
- Property tests: `tests/*_property_tests.rs` using `proptest` (100+ iterations)
- Doc examples: Cargo snippets, feature names, and package/example references in `README.md` and `docs/official_docs/` are validated against `cargo metadata` by `scripts/check-doc-examples.sh`. The PR tier compiles the standalone example workspaces and cargo-adk templates.

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

- Edition 2024, MSRV 1.95.0
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

The [PR template](.github/pull_request_template.md) carries the checklist — quality gates, code
quality, hygiene, and documentation. Complete it when you open your PR, and reference the issue it
addresses (`Fixes #123`).

## Architecture Notes

### Adding a New LLM Provider

1. Implement `adk_core::Llm` trait in `adk-model/src/<provider>/`
2. Add feature flag to `adk-model/Cargo.toml`
3. Re-export from `adk-model/src/lib.rs` behind the feature
4. Add an example in [adk-playground](https://github.com/zavora-ai/adk-playground) or as a standalone crate in `examples/`

### Adding a New Tool

1. Implement `adk_core::Tool` trait
2. Add to `adk-tool/src/` or create a new crate if it has heavy deps
3. Write tests (unit + property if applicable)
4. Add an example demonstrating usage

### Adding a New Example

Add examples to the [adk-playground](https://github.com/zavora-ai/adk-playground) repo. For integration validation examples that need to live in this repository, create a standalone crate outside the root workspace:

```
examples/<name>/
├── Cargo.toml      # [workspace] key, path dependencies to workspace crates
├── Cargo.lock
├── src/main.rs
├── README.md
└── .env.example    # the environment variables the example reads
```

### adk-mistralrs

`adk-mistralrs` is published to crates.io as a workspace member. GPU features (`cuda`, `metal`) are opt-in. Build it with:

```bash
cargo build -p adk-mistralrs
cargo build -p adk-mistralrs --features metal  # macOS GPU
cargo build -p adk-mistralrs --features cuda   # NVIDIA GPU
```

## Release Process

Release from a clean checkout after all other intended changes have merged. The
root workspace version is the source of truth for every publishable crate. During
the cycle the README describes the version as `release candidate — unpublished`;
the release-preparation pull request switches it to its final, released state, so
the tagged commit is exactly what crates.io receives. Publishing is always the last
step.

1. If the workspace is not already at the target version, run
   `python3 scripts/bump-version.py <version>`. Add an empty `## [Unreleased]`
   section above the dated release, and make the changelog comparison links point
   from the new version to `HEAD` and from the previous tag to the new tag.
2. Assemble the changelog fragments into the release heading and commit the result
   together with the removed fragment files:

   ```bash
   bash scripts/changelog-assemble.sh --into <version>
   ```

3. In the same pull request, set the README banner to `v<version> Released!` with
   the crates.io link for the new version, and mark the roadmap version `(current)`.
   Run the normal quality gates plus:

   ```bash
   bash scripts/check-release-consistency.sh
   bash scripts/check-publish-order.sh
   cargo xtask publish --dry-run
   ```

   The publish dry-run must run from a clean checkout without ignored build
   artifacts inside crate directories. A warning about yanked `spin 0.9.x` is
   currently expected through upstream `flume/sqlx` and `heapless/postcard`
   dependency chains; it does not prevent packaging.
4. Merge the preparation pull request and wait for both the PR tier and the
   post-merge macOS/Windows tier to pass on the exact release commit.
5. Create an annotated tag and validate the tagged checkout before pushing it.
   Release mode fails while the README still describes a release candidate:

   ```bash
   git tag -a v<version> -m "Release <version>"
   bash scripts/check-release-consistency.sh --release
   git push origin v<version>
   ```

6. Create the GitHub release from the annotated tag, using the matching changelog
   entry as the release notes.
7. Publish with `cargo xtask publish`. If crates.io indexing interrupts the
   workspace publish, resume safely with `cargo xtask publish --resume`; it uses
   the computed dependency order and skips versions that already exist. Then
   confirm every crate's new version on crates.io and its docs.rs build.

## Getting Help

- [GitHub Issues](https://github.com/zavora-ai/adk-rust/issues) — bug reports and feature requests
- [GitHub Discussions](https://github.com/zavora-ai/adk-rust/discussions) — questions and ideas

## License

By contributing, you agree that your contributions will be licensed under the [Apache 2.0 License](LICENSE).
