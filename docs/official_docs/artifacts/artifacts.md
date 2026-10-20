# Artifacts

Artifacts provide a way to store and retrieve binary data (images, PDFs, audio files, etc.) within your agent applications. The artifact system handles versioning, namespace scoping, and persistence of data across sessions.

## Overview

The artifact system in ADK-Rust consists of:

- **Part**: The core data representation that can hold text or binary data with MIME types
- **ArtifactService**: The trait defining artifact storage operations
- **InMemoryArtifactService**: An in-memory implementation for development and testing
- **FileArtifactService** and **GcsArtifactService** (feature `gcs`): durable implementations
- **ScopedArtifacts**: A wrapper that simplifies artifact operations by automatically handling session context

Artifacts are scoped by application, user, and session, providing isolation and organization. Files can be session-scoped (default) or user-scoped (using the `user:` prefix).

## Part Representation

An artifact is one `adk_core::Part`. Two variants carry artifact data:

| Variant | Use | Build with |
|---------|-----|------------|
| `Part::Text { text }` | Text data | `Part::text_part("...")` |
| `Part::InlineData { mime_type, data, uri, annotations }` | Binary data with a MIME type | `Part::inline_data("image/png", bytes)` |

`Part::inline_data` panics when `data` exceeds `MAX_INLINE_DATA_SIZE` (10 MB); build the
variant directly to avoid the check. Match it with `Part::InlineData { mime_type, data, .. }`.

## Basic Usage

The simplest way to work with artifacts is through the `Artifacts` trait, which tool and callback
contexts expose when the runner has an artifact service
(`Runner::builder().artifact_service(...)`):

```rust
use adk_core::{AdkError, Part, Result, ToolContext};
use serde_json::{Value, json};

// In a tool
async fn save_report(ctx: &dyn ToolContext) -> Result<Value> {
    let artifacts = ctx.artifacts().ok_or_else(|| AdkError::artifact("no artifact service"))?;

    // Save text data
    let version = artifacts.save("report.txt", &Part::text_part("Report content")).await?;

    // Save binary data
    let image_data = vec![0xFF, 0xD8, 0xFF]; // JPEG header
    artifacts.save("chart.jpg", &Part::inline_data("image/jpeg", image_data)).await?;

    Ok(json!({ "saved": true, "version": version }))
}
```

`artifacts()` returns `None` when no artifact service is configured.

## ArtifactService Trait

The `ArtifactService` trait defines the core operations for artifact management:

```rust
#[async_trait]
pub trait ArtifactService: Send + Sync {
    async fn save(&self, req: SaveRequest) -> Result<SaveResponse>;
    async fn load(&self, req: LoadRequest) -> Result<LoadResponse>;
    async fn delete(&self, req: DeleteRequest) -> Result<()>;
    async fn list(&self, req: ListRequest) -> Result<ListResponse>;
    async fn versions(&self, req: VersionsRequest) -> Result<VersionsResponse>;
    async fn health_check(&self) -> Result<()> { Ok(()) }
}
```

File names are logical keys: an empty name, `/`, `\`, and `..` are refused.

### Save Operation

Save an artifact with automatic or explicit versioning:

```rust
use adk_artifact::{ArtifactService, InMemoryArtifactService, SaveRequest};
use adk_core::Part;

let service = InMemoryArtifactService::new();
let pdf_bytes = std::fs::read("document.pdf")?;

let response = service.save(SaveRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_456".to_string(),
    file_name: "document.pdf".to_string(),
    part: Part::inline_data("application/pdf", pdf_bytes),
    version: None, // Auto-increment version
}).await?;

println!("Saved as version: {}", response.version);
```

### Load Operation

Load the latest version or a specific version:

```rust
use adk_artifact::{ArtifactService, LoadRequest};
use adk_core::Part;

// Load latest version
let response = service.load(LoadRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_456".to_string(),
    file_name: "document.pdf".to_string(),
    version: None, // Load latest
}).await?;

// Load specific version
let response = service.load(LoadRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_456".to_string(),
    file_name: "document.pdf".to_string(),
    version: Some(2), // Load version 2
}).await?;

if let Part::InlineData { mime_type, data, .. } = &response.part {
    println!("Loaded {} bytes of {}", data.len(), mime_type);
}
```

### List Operation

List all artifacts in a session:

```rust
use adk_artifact::{ArtifactService, ListRequest};

let response = service.list(ListRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_456".to_string(),
}).await?;

for file_name in response.file_names {
    println!("Found artifact: {}", file_name);
}
```

### Delete Operation

Delete a specific version or all versions:

```rust
use adk_artifact::{ArtifactService, DeleteRequest};

// Delete specific version
service.delete(DeleteRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_456".to_string(),
    file_name: "document.pdf".to_string(),
    version: Some(1), // Delete version 1
}).await?;

// Delete all versions
service.delete(DeleteRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_456".to_string(),
    file_name: "document.pdf".to_string(),
    version: None, // Delete all versions
}).await?;
```

### Versions Operation

List all versions of an artifact:

```rust
use adk_artifact::{ArtifactService, VersionsRequest};

let response = service.versions(VersionsRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_456".to_string(),
    file_name: "document.pdf".to_string(),
}).await?;

println!("Available versions: {:?}", response.versions);
// Output: [3, 2, 1] (sorted newest first)
```

`load` of a missing artifact or version, and `versions` of a missing artifact, fail with an
error.

## Versioning

Artifacts support automatic versioning:

- When saving without specifying a version, the system auto-increments from the latest version
- Version 1 is assigned to the first save (version 0 on `GcsArtifactService`, matching adk-python)
- Each subsequent save increments the version number
- You can load, delete, or query specific versions

Every backend keeps the same versioning contract:

| Rule | Behaviour |
|------|-----------|
| **Versions are immutable** | A save never replaces a stored version. A save with `version: Some(n)` for an existing `n` fails with `ErrorCategory::Conflict` (`artifact.version_exists`). |
| **Concurrent saves each get a version** | Saves of one artifact that run at the same time without a version return distinct versions, and every one is stored. |
| **Distinct names are distinct artifacts** | No two file names share storage, so `a:b` and `a__b` are different artifacts. |

Every backend reports the same failures with the same category and code, so a caller can
branch on `AdkError::is_not_found()` or on `code` whichever backend is configured:

| Failure | Category | Code |
|---------|----------|------|
| `load` or `versions` of an artifact, or a version, that is not stored | `NotFound` | `artifact.not_found` |
| A file name that is empty or contains a path separator or a `..` traversal | `InvalidInput` | `artifact.invalid_name` |
| An app name, user ID, or session ID that cannot address storage | `InvalidInput` | `artifact.invalid_identifier` |
| The session ID `user`, reserved for user-scoped artifacts | `InvalidInput` | `artifact.reserved_session_id` |
| A save naming a version that already exists | `Conflict` | `artifact.version_exists` |

Storage failures keep a backend-specific code: `artifact.file.io` for a filesystem error in
`FileArtifactService`, and `artifact.gcs.*` for a GCS request that fails, carrying the
upstream status.

```rust
use adk_artifact::{ArtifactService, InMemoryArtifactService, LoadRequest, SaveRequest};
use adk_core::{ErrorCategory, Part};

let service = InMemoryArtifactService::new();
let save = |text: &str, version: Option<i64>| SaveRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_456".to_string(),
    file_name: "data.json".to_string(),
    part: Part::text_part(text),
    version,
};
let load = |version: Option<i64>| LoadRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_456".to_string(),
    file_name: "data.json".to_string(),
    version,
};

// First save - becomes version 1
assert_eq!(service.save(save("v1 data", None)).await?.version, 1);
// Second save - becomes version 2
assert_eq!(service.save(save("v2 data", None)).await?.version, 2);

// Load latest (version 2) and a specific version
let latest = service.load(load(None)).await?;
let old = service.load(load(Some(1))).await?;
assert_eq!(latest.part.text(), Some("v2 data"));
assert_eq!(old.part.text(), Some("v1 data"));

// A stored version is never replaced
let error = service.save(save("overwrite", Some(1))).await.unwrap_err();
assert_eq!(error.category, ErrorCategory::Conflict);
assert_eq!(error.code, "artifact.version_exists");
```

## Namespace Scoping

Artifacts can be scoped at two levels:

### Session-Scoped (Default)

By default, artifacts are scoped to a specific session. Each session has its own isolated artifact namespace:

```rust
use adk_artifact::{ArtifactService, SaveRequest};
use adk_core::Part;

let in_session = |session_id: &str| SaveRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: session_id.to_string(),
    file_name: "notes.txt".to_string(),
    part: Part::text_part("notes"),
    version: None,
};

// Session 1
service.save(in_session("session_1")).await?;
// Session 2 - different artifact with same name
service.save(in_session("session_2")).await?;

// These are two separate artifacts
```

### User-Scoped

Artifacts with the `user:` prefix are shared across all sessions for a user:

```rust
use adk_artifact::{ArtifactService, LoadRequest, SaveRequest};
use adk_core::Part;

// Save in session 1
service.save(SaveRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_1".to_string(),
    file_name: "user:profile.jpg".to_string(), // user: prefix
    part: Part::inline_data("image/jpeg", jpeg_bytes),
    version: None,
}).await?;

// Load in session 2 - same artifact
let profile = service.load(LoadRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_2".to_string(),
    file_name: "user:profile.jpg".to_string(),
    version: None,
}).await?;
```

The `user:` prefix enables:
- Sharing data across multiple conversations
- Persistent user preferences
- User-level caching

> **Note:** user-scoped artifacts are stored under the reserved session segment `user`
> (`adk_artifact::USER_SCOPE_SESSION_ID`). Every backend rejects `user` as a session id
> with `ErrorCategory::InvalidInput` (`artifact.reserved_session_id`), so a session's
> artifacts never appear in the listings of the user's other sessions.

## InMemoryArtifactService

The `InMemoryArtifactService` provides an in-memory implementation suitable for development and testing:

```rust
use adk_artifact::InMemoryArtifactService;
use adk_runner::Runner;
use std::sync::Arc;

let service = Arc::new(InMemoryArtifactService::new());

// Tools and callbacks of every run reach it through `ctx.artifacts()`
let runner = Runner::builder()
    .app_name("my_app")
    .agent(agent)
    .session_service(session_service)
    .artifact_service(service)
    .build()?;
```

**Note**: Data is not persisted to disk. For durable storage, use `FileArtifactService` or `GcsArtifactService` (feature `gcs`), or implement a custom `ArtifactService`.

## FileArtifactService

`FileArtifactService` stores each version as one JSON file under a base directory:

| Scope | Path |
|-------|------|
| Session | `{app_name}/{user_id}/{session_id}/{name}/v{version}.json` |
| User (`user:` names) | `{app_name}/{user_id}/user/{name}/v{version}.json` |

- **Names** — `{name}` is the whole artifact name, a `user:` prefix included (`user%3Aprofile.txt`), with `%`, `:`, the other characters Windows forbids in file names (`<>"|?*` and control characters), and the second `_` of every `__` written as `%XX` escapes. The encoding is injective, so no two names share a directory.
- **Crash safety** — a save writes the version to a temporary file (`.save-*.tmp`), flushes it to disk, and links it into place without replacing an existing file. A crash mid-save leaves the earlier versions intact; the leftover temporary file is ignored. On filesystems without hard links (FAT, exFAT) the version file is created exclusively and written in place instead.
- **Earlier layout** — artifacts written by releases that stored `:` as `__` and kept user-scoped artifacts under `_user_scoped_` are still loaded, listed, versioned, and deleted. New versions of them go to the current layout and continue their numbering. A name saved under the earlier layout with `:` or `__` is found under both spellings, as it was before.

```rust
use adk_artifact::{ArtifactService, FileArtifactService, SaveRequest};
use adk_core::Part;

#[tokio::main]
async fn main() -> adk_core::Result<()> {
    let service = FileArtifactService::new("./artifacts")?;
    let saved = service
        .save(SaveRequest {
            app_name: "my_app".to_string(),
            user_id: "user_123".to_string(),
            session_id: "session_456".to_string(),
            file_name: "report:q1".to_string(),
            part: Part::Text { text: "quarterly numbers".to_string() },
            version: None,
        })
        .await?;
    // Stored at ./artifacts/my_app/user_123/session_456/report%3Aq1/v1.json
    assert_eq!(saved.version, 1);
    Ok(())
}
```

## GcsArtifactService

`GcsArtifactService` (feature `gcs`) uses adk-python's blob names: `{app_name}/{user_id}/{session_id}/{filename}/{version}`, and `{app_name}/{user_id}/user/{filename}/{version}` for `user:` names. Every upload carries the `ifGenerationMatch=0` precondition, so GCS creates an object only when that version does not exist yet; a save that loses the race to a concurrent save moves on to the next version.

## ScopedArtifacts

The `ScopedArtifacts` wrapper simplifies artifact operations by automatically injecting session context:

```rust
use adk_artifact::{InMemoryArtifactService, ScopedArtifacts};
use adk_core::{Artifacts, Part};
use std::sync::Arc;

let service = Arc::new(InMemoryArtifactService::new());

let artifacts = ScopedArtifacts::new(
    service,
    "my_app".to_string(),
    "user_123".to_string(),
    "session_456".to_string(),
);

// Simple API - no need to specify app/user/session
let version = artifacts.save("file.txt", &Part::text_part("content")).await?;

let part = artifacts.load("file.txt").await?;
let files = artifacts.list().await?;
```

This is the same interface available through `ToolContext::artifacts()` and `CallbackContext::artifacts()`.

## Common Patterns

### Image Analysis with Multimodal Models

When you want an LLM to analyze an image stored as an artifact, you need to use a **BeforeModel callback** to inject the image directly into the LLM request. This follows the adk-go pattern.

**Why not use a tool?** Tool responses in LLM APIs are JSON text. If a tool returns image data (even base64-encoded), the model sees it as text, not as an actual image. For true multimodal analysis, the image must be included as a `Part::InlineData` in the conversation content.

```rust
use adk_agent::LlmAgentBuilder;
use adk_artifact::{ArtifactService, InMemoryArtifactService, LoadRequest, SaveRequest};
use adk_core::{BeforeModelResult, Part};
use adk_model::GeminiModel;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-3.7-flash")?);

    // Create artifact service and save an image
    let artifact_service = Arc::new(InMemoryArtifactService::new());
    let image_bytes = std::fs::read("photo.png")?;

    artifact_service.save(SaveRequest {
        app_name: "image_app".to_string(),
        user_id: "user".to_string(),
        session_id: "init".to_string(),
        file_name: "user:photo.png".to_string(),  // user-scoped for cross-session access
        part: Part::inline_data("image/png", image_bytes),
        version: None,
    }).await?;

    // Clone for use in callback
    let callback_service = artifact_service.clone();

    let agent = LlmAgentBuilder::new("image_analyst")
        .description("Analyzes images")
        .instruction("You are an image analyst. Describe what you see in the image.")
        .model(model)
        // Use BeforeModel callback to inject image into the request
        .before_model_callback(Box::new(move |_ctx, mut request| {
            let service = callback_service.clone();
            Box::pin(async move {
                // Load the image artifact
                if let Ok(response) = service.load(LoadRequest {
                    app_name: "image_app".to_string(),
                    user_id: "user".to_string(),
                    session_id: "init".to_string(),
                    file_name: "user:photo.png".to_string(),
                    version: None,
                }).await {
                    // Inject image into the last user content
                    if let Some(last_content) = request.contents.last_mut()
                        && last_content.role == "user"
                    {
                        last_content.parts.push(response.part);
                    }
                }

                // Continue with the modified request
                Ok(BeforeModelResult::Continue(request))
            })
        }))
        .build()?;

    // Now when users ask "What's in the image?", the model will see the actual image
    Ok(())
}
```

**Key points:**
- Use `BeforeModelResult::Continue(request)` to pass the modified request to the model
- Use `BeforeModelResult::Skip(response)` if you want to return a cached response instead
- The image is injected as `Part::InlineData`, which Gemini interprets as actual image data
- Use `user:` prefix for artifacts that should be accessible across sessions

### PDF Document Analysis

Gemini models can process PDF documents natively using the same BeforeModel callback pattern. PDFs are injected with MIME type `application/pdf`:

```rust
use adk_artifact::{ArtifactService, SaveRequest};
use adk_core::Part;

// Save PDF as artifact, then load and inject it with the same callback as the image
artifact_service.save(SaveRequest {
    app_name: "image_app".to_string(),
    user_id: "user".to_string(),
    session_id: "init".to_string(),
    file_name: "user:document.pdf".to_string(),
    part: Part::inline_data("application/pdf", pdf_bytes),
    version: None,
}).await?;
```

A PDF reaches the model as an inline `application/pdf` part, which Gemini reads as a document.
`examples/artifacts_eval_memory` drives artifact saves and loads through tools on a live model.

### Storing Generated Images

```rust
use adk_core::{AdkError, Part, Result, ToolContext};
use serde_json::{Value, json};

async fn generate_and_save_image(ctx: &dyn ToolContext, image_bytes: Vec<u8>) -> Result<Value> {
    let artifacts = ctx.artifacts().ok_or_else(|| AdkError::artifact("no artifact service"))?;

    let version = artifacts
        .save("generated_image.png", &Part::inline_data("image/png", image_bytes))
        .await?;

    Ok(json!({
        "message": "Image saved",
        "file": "generated_image.png",
        "version": version
    }))
}
```

### Loading and Processing Documents

```rust
use adk_core::{AdkError, Part, Result, ToolContext};
use serde_json::{Value, json};

async fn process_document(ctx: &dyn ToolContext, filename: &str) -> Result<Value> {
    let artifacts = ctx.artifacts().ok_or_else(|| AdkError::artifact("no artifact service"))?;

    // Load the document
    match artifacts.load(filename).await? {
        Part::InlineData { mime_type, data, .. } => {
            // Process based on MIME type
            let kind = match mime_type.as_str() {
                "application/pdf" => "document",
                "image/jpeg" | "image/png" => "image",
                _ => return Err(AdkError::artifact("Unsupported type")),
            };
            Ok(json!({ "kind": kind, "bytes": data.len() }))
        }
        _ => Err(AdkError::artifact("Expected binary data")),
    }
}
```

### Version History

```rust
use adk_core::{AdkError, Result, ToolContext};
use serde_json::{Value, json};

async fn show_history(ctx: &dyn ToolContext, filename: &str) -> Result<Value> {
    let artifacts = ctx.artifacts().ok_or_else(|| AdkError::artifact("no artifact service"))?;

    // Get all files
    let files = artifacts.list().await?;
    if !files.iter().any(|name| name == filename) {
        return Ok(json!({ "error": "File not found" }));
    }

    // versions() is not on the simple Artifacts trait; use the ArtifactService directly.
    Ok(json!({ "file": filename, "exists": true }))
}
```

## API Reference

For complete API documentation, see:
- `adk_core::Artifacts` - Simple trait for agent use
- `adk_artifact::ArtifactService` - Full service trait
- `adk_artifact::InMemoryArtifactService` - In-memory implementation
- `adk_artifact::ScopedArtifacts` - Scoped wrapper

## Related

- [Sessions](../sessions/sessions.md) - Session management and lifecycle
- [Callbacks](../callbacks/callbacks.md) - Accessing artifacts in callbacks
- [Tools](../tools/function-tools.md) - Using artifacts in custom tools


---

**Previous**: [← Callbacks](../callbacks/callbacks.md) | **Next**: [Events →](../events/events.md)
