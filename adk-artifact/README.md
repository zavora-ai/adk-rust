# adk-artifact

Binary artifact storage for Rust Agent Development Kit (ADK-Rust) agents.

[![Crates.io](https://img.shields.io/crates/v/adk-artifact.svg)](https://crates.io/crates/adk-artifact)
[![Documentation](https://docs.rs/adk-artifact/badge.svg)](https://docs.rs/adk-artifact)
[![License](https://img.shields.io/crates/l/adk-artifact.svg)](../LICENSE)

## Overview

`adk-artifact` provides versioned binary data storage for the Rust Agent Development Kit ([ADK-Rust](https://github.com/zavora-ai/adk-rust)). Artifacts are useful for storing images, documents, audio, and other binary data that agents produce or consume.

Three storage backends are included:

| Backend | Type | Persistence | Use case |
|---------|------|-------------|----------|
| `InMemoryArtifactService` | In-memory `HashMap` | Process lifetime only | Development, testing |
| `FileArtifactService` | Local filesystem | Durable | Single-node production, local dev |
| `GcsArtifactService` (feature `gcs`) | Google Cloud Storage | Durable | Deployed agents, Gemini Enterprise |

All implement the `ArtifactService` trait, so you can swap backends without changing application code.

The GCS backend keeps byte-for-byte blob-name parity with adk-python's `GcsArtifactService`, so Rust agents, Python agents, and the Gemini Enterprise console read the same objects:

- Session-scoped: `{app_name}/{user_id}/{session_id}/{filename}/{version}`
- User-namespaced (filename starts with `user:`): `{app_name}/{user_id}/user/{filename}/{version}`

Auto-assigned versions start at `0` (matching adk-python; the in-memory and file backends start at `1`). Authentication uses Application Default Credentials via `google-cloud-auth`.

> **Important:** save and load must resolve the same `app_name` — the engine ID when deployed. adk-python had a save/load path mismatch (googleapis/python-aiplatform#6521); this implementation takes `app_name` verbatim from each request and never rewrites it.

```toml
[dependencies]
adk-artifact = { version = "3.0.0", features = ["gcs"] }
# or via the umbrella crate:
adk-rust = { version = "3.0.0", features = ["minimal", "gcs-artifacts"] }
```

```rust,no_run
use adk_artifact::GcsArtifactService;

fn main() -> adk_core::Result<()> {
    let _service = GcsArtifactService::new_with_adc("my-artifact-bucket")?;
    Ok(())
}
```

## Installation

```toml
[dependencies]
adk-artifact = "3.0.0"
```

Or via the umbrella crate:

```toml
[dependencies]
adk-rust = { version = "3.0.0", features = ["artifacts"] }
```

## Quick Start

```rust
use adk_artifact::{ArtifactService, InMemoryArtifactService, LoadRequest, SaveRequest};
use adk_core::Part;

#[tokio::main]
async fn main() -> adk_core::Result<()> {
    let service = InMemoryArtifactService::new();
    let pdf_bytes = b"%PDF-1.7".to_vec();

    // Save an artifact (version auto-increments from 1)
    let saved = service
        .save(SaveRequest {
            app_name: "my_app".to_string(),
            user_id: "user_123".to_string(),
            session_id: "session_456".to_string(),
            file_name: "report.pdf".to_string(),
            part: Part::inline_data("application/pdf", pdf_bytes),
            version: None,
        })
        .await?;
    assert_eq!(saved.version, 1);

    // Load the latest version
    let loaded = service
        .load(LoadRequest {
            app_name: "my_app".to_string(),
            user_id: "user_123".to_string(),
            session_id: "session_456".to_string(),
            file_name: "report.pdf".to_string(),
            version: None, // None = latest
        })
        .await?;
    assert_eq!(loaded.part.mime_type(), Some("application/pdf"));
    Ok(())
}
```

## Storage Backends

### InMemoryArtifactService

Stores artifacts in a thread-safe `HashMap` behind `RwLock`. Data is lost when the process exits.

```rust
use adk_artifact::InMemoryArtifactService;

fn main() {
    let _service = InMemoryArtifactService::new();
}
```

### FileArtifactService

Persists artifacts to the local filesystem. Each version is stored as a JSON-serialized file under a directory hierarchy:

```text
<base_dir>/<app_name>/<user_id>/<session_id>/<name>/v1.json
                                                    v2.json
<base_dir>/<app_name>/<user_id>/user/<name>/v1.json        (user: names)
```

`<name>` is the artifact name with `%`, `:`, the other characters Windows forbids in file names, and the second `_` of every `__` written as `%XX` escapes, so `a:b` and `a__b` are stored apart. A save writes the version to a temporary file, flushes it, and links it into place without replacing an existing file, so a crash never leaves a partial version. Artifacts written in the earlier layout (`:` stored as `__`, user scope under `_user_scoped_`) stay loadable.

```rust
use adk_artifact::FileArtifactService;

fn main() -> adk_core::Result<()> {
    let _service = FileArtifactService::new("/var/data/artifacts")?;
    Ok(())
}
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

Every artifact supports multiple versions. When saving with `version: None`, the version auto-increments starting from 1. You can also write to an explicit version that does not exist yet:

```rust
use adk_artifact::{ArtifactService, InMemoryArtifactService, SaveRequest, VersionsRequest};
use adk_core::{ErrorCategory, Part};

#[tokio::main]
async fn main() -> adk_core::Result<()> {
    let service = InMemoryArtifactService::new();
    let req = SaveRequest {
        app_name: "app".into(),
        user_id: "user".into(),
        session_id: "sess".into(),
        file_name: "report.pdf".into(),
        part: Part::text_part("draft"),
        version: None,
    };

    // Auto-increment: first save → v1, second save → v2
    service.save(req.clone()).await?;
    service.save(req.clone()).await?;

    // Explicit version that does not exist yet
    service.save(SaveRequest { version: Some(42), ..req.clone() }).await?;

    // An existing version is never replaced
    let err = service.save(SaveRequest { version: Some(2), ..req }).await.unwrap_err();
    assert_eq!(err.category, ErrorCategory::Conflict);

    // List all versions (descending)
    let resp = service
        .versions(VersionsRequest {
            app_name: "app".into(),
            user_id: "user".into(),
            session_id: "sess".into(),
            file_name: "report.pdf".into(),
        })
        .await?;
    assert_eq!(resp.versions, vec![42, 2, 1]);
    Ok(())
}
```

Every backend keeps the same contract:

| Rule | Behaviour |
|------|-----------|
| **Versions are immutable** | A save never replaces a stored version; an explicit existing version fails with `ErrorCategory::Conflict` (`artifact.version_exists`). |
| **Concurrent saves each get a version** | Simultaneous saves without a version return distinct versions, and all are stored. |
| **Distinct names are distinct artifacts** | No two file names share storage. |
| **`user` is not a session id** | It names the user scope; every operation rejects it with `ErrorCategory::InvalidInput` (`artifact.reserved_session_id`). |

## Scoping

Artifacts are scoped by three dimensions: `app_name`, `user_id`, and `session_id`. This means two sessions for the same user can each have a `report.pdf` without conflict.

### User-Scoped Artifacts

Prefix a filename with `user:` to make it accessible across all sessions for that user:

```rust
use adk_artifact::{ArtifactService, InMemoryArtifactService, ListRequest, LoadRequest, SaveRequest};
use adk_core::Part;

#[tokio::main]
async fn main() -> adk_core::Result<()> {
    let service = InMemoryArtifactService::new();

    // Save from session A
    service
        .save(SaveRequest {
            app_name: "app".into(),
            user_id: "user_123".into(),
            session_id: "session_A".into(),
            file_name: "user:profile.png".into(),
            part: Part::inline_data("image/png", vec![0x89, 0x50, 0x4E, 0x47]),
            version: None,
        })
        .await?;

    // Load from session B — same artifact
    service
        .load(LoadRequest {
            app_name: "app".into(),
            user_id: "user_123".into(),
            session_id: "session_B".into(),
            file_name: "user:profile.png".into(),
            version: None,
        })
        .await?;

    // list() from any session returns user-scoped artifacts too
    let resp = service
        .list(ListRequest {
            app_name: "app".into(),
            user_id: "user_123".into(),
            session_id: "session_B".into(),
        })
        .await?;
    assert!(resp.file_names.contains(&"user:profile.png".to_string()));
    Ok(())
}
```

Internally, user-scoped artifacts are stored under the reserved session segment `user` (`USER_SCOPE_SESSION_ID`) in every backend, so they're visible regardless of which session ID is used. Because the segment is reserved, every backend rejects `user` as a session id.

## Deleting Artifacts

```rust
use adk_artifact::{ArtifactService, DeleteRequest};

async fn delete(service: &dyn ArtifactService) -> adk_core::Result<()> {
    let one_version = DeleteRequest {
        app_name: "app".into(),
        user_id: "user".into(),
        session_id: "sess".into(),
        file_name: "report.pdf".into(),
        version: Some(1),
    };
    // Delete a specific version
    service.delete(one_version.clone()).await?;

    // Delete all versions
    service.delete(DeleteRequest { version: None, ..one_version }).await?;
    Ok(())
}
```

Deleting a non-existent artifact or version is a no-op (no error returned).

## ScopedArtifacts

`ScopedArtifacts` wraps an `ArtifactService` and implements the simpler `adk_core::Artifacts` trait by automatically injecting `app_name`, `user_id`, and `session_id` into every request. This is the interface agents use at runtime.

```rust
use std::sync::Arc;

use adk_artifact::{InMemoryArtifactService, ScopedArtifacts};
use adk_core::{Artifacts, Part};

#[tokio::main]
async fn main() -> adk_core::Result<()> {
    let service = Arc::new(InMemoryArtifactService::new());
    let artifacts = ScopedArtifacts::new(
        service,
        "my_app".to_string(),
        "user_123".to_string(),
        "session_456".to_string(),
    );

    // Three-method API
    let part = Part::inline_data("image/png", vec![0x89, 0x50, 0x4E, 0x47]);
    let version = artifacts.save("chart.png", &part).await?;
    let _loaded = artifacts.load("chart.png").await?;
    let files = artifacts.list().await?;
    assert_eq!((version, files), (1, vec!["chart.png".to_string()]));
    Ok(())
}
```

## File Name Validation

Every backend validates artifact file names to prevent path traversal:

| Backend | Rejected names |
|---------|----------------|
| `InMemoryArtifactService`, `FileArtifactService` | Empty; containing `/` or `\`; equal to `.` or `..`; containing `..` anywhere |
| `GcsArtifactService` | Empty; starting or ending with `/`; containing `\`; with an empty, `.`, or `..` path component |

`FileArtifactService` also rejects an app name, user id, or session id that is empty or contains a separator or traversal pattern.

## Use with LoadArtifactsTool

Artifacts integrate with agents via `LoadArtifactsTool` from `adk-tool`:

```rust
use std::sync::Arc;

use adk_agent::{LlmAgent, LlmAgentBuilder};
use adk_core::{Llm, Result};
use adk_tool::LoadArtifactsTool;

fn assistant(model: Arc<dyn Llm>) -> Result<LlmAgent> {
    LlmAgentBuilder::new("assistant")
        .model(model)
        .tool(Arc::new(LoadArtifactsTool::new()))
        .build()
}
```

The LLM can then call this tool to load artifacts by name into the conversation.

## Changes in 3.0

| Change | Migration |
|--------|-----------|
| A save naming an existing version fails with `artifact.version_exists` (`Conflict`) on every backend | Save with `version: None` to append |
| The session id `user` (`USER_SCOPE_SESSION_ID`) is rejected with `artifact.reserved_session_id` | Use another session id; artifacts saved earlier under session `user` are no longer reachable |
| `FileArtifactService` encodes names injectively and stores user-scoped artifacts under `{app}/{user}/user/` | Tools reading the directory tree directly need the new paths; the earlier layout stays readable |
| Concurrent saves each keep their own version; file writes are crash-safe | — |

See the [2.x to 3.0 migration guide](../docs/official_docs/migration/2.x-to-3.0.md) and the [changelog](../CHANGELOG.md) for the full list.

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) — Umbrella crate
- [adk-core](https://crates.io/crates/adk-core) — `Artifacts` trait, `Part` type
- [adk-tool](https://crates.io/crates/adk-tool) — `LoadArtifactsTool`

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
