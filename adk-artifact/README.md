# adk-artifact

Binary artifact storage for Rust Agent Development Kit (ADK-Rust) agents.

[![Crates.io](https://img.shields.io/crates/v/adk-artifact.svg)](https://crates.io/crates/adk-artifact)
[![Documentation](https://docs.rs/adk-artifact/badge.svg)](https://docs.rs/adk-artifact)
[![License](https://img.shields.io/crates/l/adk-artifact.svg)](LICENSE)

## Overview

`adk-artifact` provides binary data storage for the Rust Agent Development Kit ([ADK-Rust](https://github.com/zavora-ai/adk-rust)):

- **InMemoryArtifactService** - Simple in-memory artifact storage
- **ArtifactService** - Trait for custom storage backends
- **ScopedArtifacts** - Session-scoped artifact access
- **Versioning** - Multiple versions per artifact

Artifacts are useful for storing images, documents, audio, and other binary data that agents produce or consume.

## Installation

```toml
[dependencies]
adk-artifact = "0.3.0"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "0.3.0", features = ["artifacts"] }
```

## Quick Start

```rust
use adk_artifact::{InMemoryArtifactService, ArtifactService, SaveRequest, LoadRequest};
use adk_core::Part;

// Create artifact service
let service = InMemoryArtifactService::new();

// Store an artifact
let response = service.save(SaveRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_456".to_string(),
    file_name: "report.pdf".to_string(),
    part: Part::InlineData {
        mime_type: "application/pdf".to_string(),
        data: pdf_bytes,
    },
    version: None, // Auto-increment
}).await?;

println!("Saved as version: {}", response.version);

// Retrieve artifact
let response = service.load(LoadRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_456".to_string(),
    file_name: "report.pdf".to_string(),
    version: None, // Latest version
}).await?;
```

## Use with LoadArtifactsTool

Artifacts integrate with agents via `LoadArtifactsTool`:

```rust
use adk_tool::LoadArtifactsTool;

let tool = LoadArtifactsTool::new();

let agent = LlmAgentBuilder::new("assistant")
    .tool(Arc::new(tool))
    .build()?;
```

The LLM can then call this tool to load artifacts by name into the conversation.

## Features

- Async storage and retrieval
- Automatic MIME type detection
- Version history support
- Thread-safe concurrent access
- User-scoped artifacts (use `user:` prefix for cross-session access)

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-core](https://crates.io/crates/adk-core) - Core traits
- [adk-tool](https://crates.io/crates/adk-tool) - `LoadArtifactsTool`

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
