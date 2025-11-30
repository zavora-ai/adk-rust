# adk-artifact

Binary artifact storage for ADK agents.

[![Crates.io](https://img.shields.io/crates/v/adk-artifact.svg)](https://crates.io/crates/adk-artifact)
[![Documentation](https://docs.rs/adk-artifact/badge.svg)](https://docs.rs/adk-artifact)
[![License](https://img.shields.io/crates/l/adk-artifact.svg)](LICENSE)

## Overview

`adk-artifact` provides binary data storage for the Rust Agent Development Kit ([ADK-Rust](https://github.com/zavora-ai/adk-rust)):

- **InMemoryArtifactService** - Simple in-memory artifact storage
- **Artifact Management** - Store, retrieve, and list artifacts
- **MIME Types** - Automatic content type handling
- **Versioning** - Multiple versions per artifact

Artifacts are useful for storing images, documents, audio, and other binary data that agents produce or consume.

## Installation

```toml
[dependencies]
adk-artifact = "0.1"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "0.1", features = ["artifacts"] }
```

## Quick Start

```rust
use adk_artifact::InMemoryArtifactService;

// Create artifact service
let service = InMemoryArtifactService::new();

// Store an artifact
let artifact = service.save_artifact(
    "app_name",
    "user_123",
    "session_456",
    "report.pdf",
    pdf_bytes,
).await?;

// Retrieve artifact
let data = service.load_artifact(
    "app_name",
    "user_123",
    "session_456",
    "report.pdf",
    None, // Latest version
).await?;
```

## Use with LoadArtifactsTool

Artifacts integrate with agents via `LoadArtifactsTool`:

```rust
use adk_tool::LoadArtifactsTool;

let tool = LoadArtifactsTool::new(vec!["image.png", "document.pdf"]);

let agent = LlmAgentBuilder::new("assistant")
    .tool(Arc::new(tool))
    .build()?;
```

The LLM can then request artifacts to be loaded into context.

## Features

- Async storage and retrieval
- Automatic MIME type detection
- Version history support
- Thread-safe concurrent access

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-core](https://crates.io/crates/adk-core) - Core `Artifacts` trait
- [adk-tool](https://crates.io/crates/adk-tool) - `LoadArtifactsTool`

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://github.com/zavora-ai/adk-rust) framework for building AI agents in Rust.
