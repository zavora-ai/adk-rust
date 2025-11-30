# ADK-Rust Crates.io Publication Plan

This document outlines the steps required to publish ADK-Rust packages to crates.io.

## Current Status

### What's Ready
- **LICENSE**: Apache-2.0 license file exists at project root
- **Workspace metadata**: version (0.1.0), edition (2021), rust-version (1.75), license, authors defined
- **Main crate (adk-rust)**: Has complete metadata (description, repository, keywords, categories, readme)
- **Main crate documentation**: `adk-rust/src/lib.rs` has comprehensive crate-level docs with examples
- **Feature flags**: Well-organized with `default`, `full`, `minimal` presets

### Completed Tasks

#### ✅ Package Metadata
All packages now have complete crates.io metadata:

| Package | description | repository | documentation | keywords | categories |
|---------|-------------|------------|---------------|----------|------------|
| adk-core | ✅ | ✅ | ✅ | ✅ | ✅ |
| adk-agent | ✅ | ✅ | ✅ | ✅ | ✅ |
| adk-model | ✅ | ✅ | ✅ | ✅ | ✅ |
| adk-tool | ✅ | ✅ | ✅ | ✅ | ✅ |
| adk-session | ✅ | ✅ | ✅ | ✅ | ✅ |
| adk-artifact | ✅ | ✅ | ✅ | ✅ | ✅ |
| adk-memory | ✅ | ✅ | ✅ | ✅ | ✅ |
| adk-runner | ✅ | ✅ | ✅ | ✅ | ✅ |
| adk-server | ✅ | ✅ | ✅ | ✅ | ✅ |
| adk-cli | ✅ | ✅ | ✅ | ✅ | ✅ |
| adk-telemetry | ✅ | ✅ | ✅ | ✅ | ✅ |

#### ✅ README Files
All packages have README.md files:

- [x] adk-core/README.md
- [x] adk-agent/README.md
- [x] adk-model/README.md
- [x] adk-tool/README.md
- [x] adk-session/README.md
- [x] adk-artifact/README.md
- [x] adk-memory/README.md
- [x] adk-runner/README.md
- [x] adk-server/README.md
- [x] adk-cli/README.md
- [x] adk-telemetry/README.md

#### ✅ Crate-Level Documentation
All `lib.rs` files have `//!` documentation:

- [x] adk-core/src/lib.rs
- [x] adk-agent/src/lib.rs
- [x] adk-model/src/lib.rs
- [x] adk-tool/src/lib.rs
- [x] adk-session/src/lib.rs
- [x] adk-artifact/src/lib.rs
- [x] adk-memory/src/lib.rs
- [x] adk-runner/src/lib.rs
- [x] adk-server/src/lib.rs
- [x] adk-cli/src/lib.rs
- [x] adk-telemetry/src/lib.rs (already had docs)

#### ✅ Version Constraints
All internal dependencies now specify `version = "0.1.0"` for crates.io compatibility.

### Blocking Issue

#### Patched Dependency: `gemini-rust`
The workspace uses a patched version of `gemini-rust` from `vendor/gemini-rust`:

```toml
[patch.crates-io]
gemini-rust = { path = "vendor/gemini-rust" }
```

**Why patched**: Added `DOCUMENT` modality support for PDF processing.

**Resolution options**:
1. **Submit PR upstream** to https://github.com/flachesis/gemini-rust (preferred)
2. **Fork and publish** as `gemini-rust-adk` to crates.io
3. **Wait for upstream** to add PDF support

**Recommendation**: Submit PR upstream first. If not merged quickly, fork and publish.

## Publication Order

Packages must be published in dependency order (leaf dependencies first):

```
Phase 1 (no internal dependencies):
  └─ adk-core
  └─ adk-telemetry

Phase 2 (depends on Phase 1):
  └─ adk-model (depends on: adk-core, adk-telemetry)
  └─ adk-session (depends on: adk-core)
  └─ adk-artifact (depends on: adk-core)
  └─ adk-memory (depends on: adk-core)

Phase 3 (depends on Phase 2):
  └─ adk-tool (depends on: adk-core, adk-model)
  └─ adk-agent (depends on: adk-core, adk-model, adk-tool, adk-telemetry)
  └─ adk-runner (depends on: adk-core, adk-model, adk-session, adk-artifact, adk-memory, adk-telemetry)

Phase 4 (depends on Phase 3):
  └─ adk-server (depends on: adk-core, adk-agent, adk-runner, adk-session, adk-artifact, adk-memory, adk-telemetry)
  └─ adk-cli (depends on: adk-core, adk-runner, adk-server, adk-session, adk-telemetry)

Phase 5 (meta-crate):
  └─ adk-rust (depends on: all above)
```

## Metadata Template

For each package, add to `Cargo.toml`:

```toml
[package]
name = "adk-{name}"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
authors.workspace = true
description = "{description}"
repository = "https://github.com/zavora-ai/adk-rust"
documentation = "https://docs.rs/adk-{name}"
keywords = ["ai", "agent", "adk", "gemini", "llm"]
categories = ["api-bindings", "asynchronous"]
readme = "README.md"
```

## Package Descriptions

| Package | Description |
|---------|-------------|
| adk-core | Core traits and types for ADK agents, tools, sessions, and events |
| adk-telemetry | OpenTelemetry integration for ADK agent observability |
| adk-model | LLM model integrations for ADK (Gemini, etc.) |
| adk-tool | Tool system for ADK agents (FunctionTool, MCP, Google Search) |
| adk-session | Session management and state persistence for ADK |
| adk-artifact | Binary artifact storage for ADK agents |
| adk-memory | Semantic memory and search for ADK agents |
| adk-agent | Agent implementations for ADK (LLM, Custom, Workflow agents) |
| adk-runner | Agent execution runtime for ADK |
| adk-server | HTTP server and A2A protocol for ADK agents |
| adk-cli | Command-line launcher for ADK agents |
| adk-rust | Agent Development Kit - Build AI agents in Rust (Google ADK) |

## README Template

Each package README should follow this structure:

```markdown
# adk-{name}

{One-line description}

[![Crates.io](https://img.shields.io/crates/v/adk-{name}.svg)](https://crates.io/crates/adk-{name})
[![Documentation](https://docs.rs/adk-{name}/badge.svg)](https://docs.rs/adk-{name})
[![License](https://img.shields.io/crates/l/adk-{name}.svg)](LICENSE)

## Overview

{2-3 paragraph description of what this crate does}

## Installation

```toml
[dependencies]
adk-{name} = "0.1"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = "0.1"
```

## Quick Example

```rust
// Simple example showing primary use case
```

## Features

- Feature 1
- Feature 2
- Feature 3

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-core](https://crates.io/crates/adk-core) - Core traits and types

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://github.com/zavora-ai/adk-rust) framework.
```

## Lib.rs Documentation Template

```rust
//! # adk-{name}
//!
//! {One-line description}
//!
//! ## Overview
//!
//! {What this crate provides}
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! // Example code
//! ```
//!
//! ## Main Types
//!
//! - [`TypeA`] - Description
//! - [`TypeB`] - Description
//!
//! ## Features
//!
//! - Feature 1
//! - Feature 2
```

## Pre-Publication Checklist

For each package:

- [ ] Cargo.toml has all required metadata
- [ ] README.md exists and is informative
- [ ] lib.rs has crate-level documentation
- [ ] All public items have doc comments
- [ ] `cargo doc --no-deps` succeeds
- [ ] `cargo publish --dry-run` succeeds
- [ ] All tests pass

## Publication Commands

```bash
# Verify everything builds
cargo build --all-features

# Run all tests
cargo test --all-features

# Generate docs
cargo doc --no-deps --all-features

# Dry-run publish (each crate)
cargo publish --dry-run -p adk-core
cargo publish --dry-run -p adk-telemetry
# ... etc

# Actual publish (in order!)
cargo publish -p adk-core
cargo publish -p adk-telemetry
cargo publish -p adk-model
cargo publish -p adk-session
cargo publish -p adk-artifact
cargo publish -p adk-memory
cargo publish -p adk-tool
cargo publish -p adk-agent
cargo publish -p adk-runner
cargo publish -p adk-server
cargo publish -p adk-cli
cargo publish -p adk-rust
```

## Timeline Estimate

1. **gemini-rust resolution**: Submit PR, wait or fork
2. **Metadata & README creation**: 11 packages × metadata + README + docs
3. **Testing**: Dry-run all packages
4. **Publication**: Sequential publish in dependency order

## Next Steps

1. Decide on gemini-rust approach (PR upstream vs fork)
2. Add metadata to all Cargo.toml files
3. Create README.md for each package
4. Add crate-level docs to each lib.rs
5. Run dry-run tests
6. Publish in order
