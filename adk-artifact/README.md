# adk-artifact

Binary artifact storage for Rust Agent Development Kit (ADK-Rust) agents.

[![Crates.io](https://img.shields.io/crates/v/adk-artifact.svg)](https://crates.io/crates/adk-artifact)
[![Documentation](https://docs.rs/adk-artifact/badge.svg)](https://docs.rs/adk-artifact)
[![License](https://img.shields.io/crates/l/adk-artifact.svg)](LICENSE)

## Overview

`adk-artifact` provides versioned binary data storage for the Rust Agent Development Kit ([ADK-Rust](https://github.com/zavora-ai/adk-rust)). Artifacts are useful for storing images, documents, audio, and other binary data that agents produce or consume.

Two storage backends are included:

| Backend | Type | Persistence | Use case |
|---------|------|-------------|----------|
| `InMemoryArtifactService` | In-memory `HashMap` | Process lifetime only | Development, testing |
| `FileArtifactService` | Local filesystem | Durable | Single-node production, local dev |

Both implement the `ArtifactService` trait, so you can swap backends without changing application code.

## Installation

```toml
[dependencies]
adk-artifact = "0.5.0"
```

Or via the umbrella crate:

```toml
[dependencies]
adk-rust = { version = "0.5.0", features = ["artifacts"] }
```

## Quick Start

```rust
use adk_artifact::{InMemoryArtifactService, ArtifactService, SaveRequest, LoadRequest};
use adk_core::Part;

let service = InMemoryArtifactService::new();

// Save an artifact (version auto-increments from 1)
let resp = service.save(SaveRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_456".to_string(),
    file_name: "report.pdf".to_string(),
    part: Part::InlineData {
        mime_type: "application/pdf".to_string(),
        data: pdf_bytes,
    },
    version: None,
}).await?;

println!("Saved as version {}", resp.version); // 1

// Load the latest version
let resp = service.load(LoadRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_456".to_string(),
    file_name: "report.pdf".to_string(),
    version: None, // None = latest
}).await?;
```

## Storage Backends

### InMemoryArtifactService

Stores artifacts in a thread-safe `HashMap` behind `RwLock`. Data is lost when the process exits.

```rust
use adk_artifact::InMemoryArtifactService;

let service = InMemoryArtifactService::new();
```

### FileArtifactService

Persists artifacts to the local filesystem. Each version is stored as a JSON-serialized file under a directory hierarchy:

```
<base_dir>/<app_name>/<user_id>/<session_id>/<file_name>/v1.json
                                                         v2.json
```

```rust
use adk_artifact::FileArtifactService;

let service = FileArtifactService::new("/var/data/artifacts");
```

`FileArtifactService` includes a `health_check()` method that verifies the base directory is writable.

## ArtifactService Trait

All backends implement the full `ArtifactService` trait:

| Method | Description |
|--------|-------------|
| `save(SaveRequest)` | Store an artifact. Auto-increments version when `version: None`. |
| `load(LoadRequest)` | Retrieve an artifact. Loads latest version when `version: None`. |
| `delete(DeleteRequest)` | Delete a specific version, or all versions when `version: None`. |
| `list(ListRequest)` | List all artifact filenames in a session (includes user-scoped artifacts). |
| `versions(VersionsRequest)` | List all available version numbers for a specific artifact. |
| `health_check()` | Verify backend connectivity. Default impl returns `Ok(())`. |

## Versioning

Every artifact supports multiple versions. When saving with `version: None`, the version auto-increments starting from 1. You can also write to an explicit version:

```rust
// Auto-increment: first save → v1, second save → v2
service.save(SaveRequest { version: None, ..req }).await?;
service.save(SaveRequest { version: None, ..req }).await?;

// Explicit version
service.save(SaveRequest { version: Some(42), ..req }).await?;

// List all versions (returned in descending order)
let resp = service.versions(VersionsRequest {
    app_name: "app".into(),
    user_id: "user".into(),
    session_id: "sess".into(),
    file_name: "report.pdf".into(),
}).await?;
// resp.versions == [2, 1]
```

## Scoping

Artifacts are scoped by three dimensions: `app_name`, `user_id`, and `session_id`. This means two sessions for the same user can each have a `report.pdf` without conflict.

### User-Scoped Artifacts

Prefix a filename with `user:` to make it accessible across all sessions for that user:

```rust
// Save from session A
service.save(SaveRequest {
    session_id: "session_A".into(),
    file_name: "user:profile.png".into(),
    ..req
}).await?;

// Load from session B — same artifact
service.load(LoadRequest {
    session_id: "session_B".into(),
    file_name: "user:profile.png".into(),
    ..req
}).await?;

// list() from any session returns user-scoped artifacts too
let resp = service.list(ListRequest {
    session_id: "session_B".into(),
    ..list_req
}).await?;
// resp.file_names includes "user:profile.png"
```

Internally, user-scoped artifacts are stored under a shared `_user_scoped_` directory (filesystem) or a `"user"` session key (in-memory), so they're visible regardless of which session ID is used.

## Deleting Artifacts

```rust
use adk_artifact::DeleteRequest;

// Delete a specific version
service.delete(DeleteRequest {
    app_name: "app".into(),
    user_id: "user".into(),
    session_id: "sess".into(),
    file_name: "report.pdf".into(),
    version: Some(1),
}).await?;

// Delete all versions
service.delete(DeleteRequest {
    version: None,
    ..del_req
}).await?;
```

Deleting a non-existent artifact or version is a no-op (no error returned).

## ScopedArtifacts

`ScopedArtifacts` wraps an `ArtifactService` and implements the simpler `adk_core::Artifacts` trait by automatically injecting `app_name`, `user_id`, and `session_id` into every request. This is the interface agents use at runtime.

```rust
use adk_artifact::{ScopedArtifacts, InMemoryArtifactService};
use adk_core::{Artifacts, Part};
use std::sync::Arc;

let service = Arc::new(InMemoryArtifactService::new());
let artifacts = ScopedArtifacts::new(
    service,
    "my_app".to_string(),
    "user_123".to_string(),
    "session_456".to_string(),
);

// Simple three-method API
let version = artifacts.save("chart.png", &part).await?;
let loaded = artifacts.load("chart.png").await?;
let files = artifacts.list().await?;
```

## File Name Validation

Both backends validate artifact filenames to prevent path traversal attacks. The following are rejected with an `AdkError::Artifact` error:

- Empty names
- Names containing `/` or `\`
- Names equal to `.` or `..`
- Names containing `..` anywhere

```rust
// These all return Err:
service.save(SaveRequest { file_name: "".into(), .. }).await;           // empty
service.save(SaveRequest { file_name: "../etc/passwd".into(), .. }).await; // traversal
service.save(SaveRequest { file_name: "foo/bar.txt".into(), .. }).await;  // path separator
```

## Use with LoadArtifactsTool

Artifacts integrate with agents via `LoadArtifactsTool` from `adk-tool`:

```rust
use adk_tool::LoadArtifactsTool;

let tool = LoadArtifactsTool::new();

let agent = LlmAgentBuilder::new("assistant")
    .tool(Arc::new(tool))
    .build()?;
```

The LLM can then call this tool to load artifacts by name into the conversation.

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) — Umbrella crate
- [adk-core](https://crates.io/crates/adk-core) — `Artifacts` trait, `Part` type
- [adk-tool](https://crates.io/crates/adk-tool) — `LoadArtifactsTool`

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
