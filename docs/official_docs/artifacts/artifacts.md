# Artifacts

Artifacts provide a way to store and retrieve binary data (images, PDFs, audio files, etc.) within your agent applications. The artifact system handles versioning, namespace scoping, and persistence of data across sessions.

## Overview

The artifact system in ADK-Rust consists of:

- **Part**: The core data representation that can hold text or binary data with MIME types
- **ArtifactService**: The trait defining artifact storage operations
- **InMemoryArtifactService**: An in-memory implementation for development and testing
- **ScopedArtifacts**: A wrapper that simplifies artifact operations by automatically handling session context

Artifacts are scoped by application, user, and session, providing isolation and organization. Files can be session-scoped (default) or user-scoped (using the `user:` prefix).

## Part Representation

The `Part` enum represents data that can be stored as artifacts:

```rust
pub enum Part {
    Text { text: String },
    InlineData { mime_type: String, data: Vec<u8> },
    FunctionCall { name: String, args: serde_json::Value },
    FunctionResponse { name: String, response: serde_json::Value },
}
```

For artifacts, you'll primarily use:
- `Part::Text` for text data
- `Part::InlineData` for binary data with MIME types

## Basic Usage

The simplest way to work with artifacts is through the `Artifacts` trait, which is available on agent contexts:

```rust
use adk_rust::prelude::*;

// In an agent tool or callback
async fn save_report(ctx: &ToolContext) -> Result<Value> {
    let artifacts = ctx.artifacts();
    
    // Save text data
    let version = artifacts.save(
        "report.txt",
        &Part::Text { text: "Report content".to_string() }
    ).await?;
    
    // Save binary data
    let image_data = vec![0xFF, 0xD8, 0xFF]; // JPEG header
    artifacts.save(
        "chart.jpg",
        &Part::InlineData {
            mime_type: "image/jpeg".to_string(),
            data: image_data,
        }
    ).await?;
    
    Ok(json!({ "saved": true, "version": version }))
}
```

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
}
```

### Save Operation

Save an artifact with automatic or explicit versioning:

```rust
use adk_artifact::{InMemoryArtifactService, SaveRequest};
use adk_core::Part;

let service = InMemoryArtifactService::new();

let response = service.save(SaveRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_456".to_string(),
    file_name: "document.pdf".to_string(),
    part: Part::InlineData {
        mime_type: "application/pdf".to_string(),
        data: pdf_bytes,
    },
    version: None, // Auto-increment version
}).await?;

println!("Saved as version: {}", response.version);
```

### Load Operation

Load the latest version or a specific version:

```rust
use adk_artifact::LoadRequest;

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

match response.part {
    Part::InlineData { mime_type, data } => {
        println!("Loaded {} bytes of {}", data.len(), mime_type);
    }
    _ => {}
}
```

### List Operation

List all artifacts in a session:

```rust
use adk_artifact::ListRequest;

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
use adk_artifact::DeleteRequest;

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
use adk_artifact::VersionsRequest;

let response = service.versions(VersionsRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_456".to_string(),
    file_name: "document.pdf".to_string(),
}).await?;

println!("Available versions: {:?}", response.versions);
// Output: [3, 2, 1] (sorted newest first)
```

## Versioning

Artifacts support automatic versioning:

- When saving without specifying a version, the system auto-increments from the latest version
- Version 1 is assigned to the first save
- Each subsequent save increments the version number
- You can load, delete, or query specific versions

```rust
// First save - becomes version 1
let v1 = service.save(SaveRequest {
    file_name: "data.json".to_string(),
    part: Part::Text { text: "v1 data".to_string() },
    version: None,
    // ... other fields
}).await?;
assert_eq!(v1.version, 1);

// Second save - becomes version 2
let v2 = service.save(SaveRequest {
    file_name: "data.json".to_string(),
    part: Part::Text { text: "v2 data".to_string() },
    version: None,
    // ... other fields
}).await?;
assert_eq!(v2.version, 2);

// Load latest (version 2)
let latest = service.load(LoadRequest {
    file_name: "data.json".to_string(),
    version: None,
    // ... other fields
}).await?;

// Load specific version
let old = service.load(LoadRequest {
    file_name: "data.json".to_string(),
    version: Some(1),
    // ... other fields
}).await?;
```

## Namespace Scoping

Artifacts can be scoped at two levels:

### Session-Scoped (Default)

By default, artifacts are scoped to a specific session. Each session has its own isolated artifact namespace:

```rust
// Session 1
service.save(SaveRequest {
    session_id: "session_1".to_string(),
    file_name: "notes.txt".to_string(),
    // ... other fields
}).await?;

// Session 2 - different artifact with same name
service.save(SaveRequest {
    session_id: "session_2".to_string(),
    file_name: "notes.txt".to_string(),
    // ... other fields
}).await?;

// These are two separate artifacts
```

### User-Scoped

Artifacts with the `user:` prefix are shared across all sessions for a user:

```rust
// Save in session 1
service.save(SaveRequest {
    session_id: "session_1".to_string(),
    file_name: "user:profile.jpg".to_string(), // user: prefix
    // ... other fields
}).await?;

// Load in session 2 - same artifact
let profile = service.load(LoadRequest {
    session_id: "session_2".to_string(),
    file_name: "user:profile.jpg".to_string(),
    // ... other fields
}).await?;
```

The `user:` prefix enables:
- Sharing data across multiple conversations
- Persistent user preferences
- User-level caching

## InMemoryArtifactService

The `InMemoryArtifactService` provides an in-memory implementation suitable for development and testing:

```rust
use adk_artifact::InMemoryArtifactService;
use std::sync::Arc;

let service = Arc::new(InMemoryArtifactService::new());

// Use with agents
let agent = LlmAgentBuilder::new("my_agent")
    .model(model)
    .build()?;

// The service can be passed to runners or used directly
```

**Note**: Data is not persisted to disk. For production use, consider implementing a custom `ArtifactService` backed by a database or cloud storage.

## ScopedArtifacts

The `ScopedArtifacts` wrapper simplifies artifact operations by automatically injecting session context:

```rust
use adk_artifact::{ScopedArtifacts, InMemoryArtifactService};
use std::sync::Arc;

let service = Arc::new(InMemoryArtifactService::new());

let artifacts = ScopedArtifacts::new(
    service,
    "my_app".to_string(),
    "user_123".to_string(),
    "session_456".to_string(),
);

// Simple API - no need to specify app/user/session
let version = artifacts.save("file.txt", &Part::Text {
    text: "content".to_string()
}).await?;

let part = artifacts.load("file.txt").await?;
let files = artifacts.list().await?;
```

This is the same interface available through `ToolContext::artifacts()` and `CallbackContext::artifacts()`.

## Common Patterns

### Image Analysis with Multimodal Models

When you want an LLM to analyze an image stored as an artifact, you need to use a **BeforeModel callback** to inject the image directly into the LLM request. This follows the adk-go pattern.

**Why not use a tool?** Tool responses in LLM APIs are JSON text. If a tool returns image data (even base64-encoded), the model sees it as text, not as an actual image. For true multimodal analysis, the image must be included as a `Part::InlineData` in the conversation content.

```rust
use adk_rust::prelude::*;
use adk_rust::artifact::{ArtifactService, InMemoryArtifactService, SaveRequest, LoadRequest};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.0-flash-exp")?);

    // Create artifact service and save an image
    let artifact_service = Arc::new(InMemoryArtifactService::new());
    let image_bytes = std::fs::read("photo.png")?;

    artifact_service.save(SaveRequest {
        app_name: "image_app".to_string(),
        user_id: "user".to_string(),
        session_id: "init".to_string(),
        file_name: "user:photo.png".to_string(),  // user-scoped for cross-session access
        part: Part::InlineData {
            data: image_bytes,
            mime_type: "image/png".to_string(),
        },
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
                    if let Some(last_content) = request.contents.last_mut() {
                        if last_content.role == "user" {
                            last_content.parts.push(response.part);
                        }
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
- Use `user:` prefix for images that should be accessible across sessions

### Storing Generated Images

```rust
async fn generate_and_save_image(ctx: &ToolContext) -> Result<Value> {
    let artifacts = ctx.artifacts();
    
    // Generate image (pseudo-code)
    let image_bytes = generate_image().await?;
    
    let version = artifacts.save(
        "generated_image.png",
        &Part::InlineData {
            mime_type: "image/png".to_string(),
            data: image_bytes,
        }
    ).await?;
    
    Ok(json!({
        "message": "Image saved",
        "file": "generated_image.png",
        "version": version
    }))
}
```

### Loading and Processing Documents

```rust
async fn process_document(ctx: &ToolContext, filename: &str) -> Result<Value> {
    let artifacts = ctx.artifacts();
    
    // Load the document
    let part = artifacts.load(filename).await?;
    
    match part {
        Part::InlineData { mime_type, data } => {
            // Process based on MIME type
            let result = match mime_type.as_str() {
                "application/pdf" => process_pdf(&data)?,
                "image/jpeg" | "image/png" => process_image(&data)?,
                _ => return Err(AdkError::Artifact("Unsupported type".into())),
            };
            
            Ok(json!({ "result": result }))
        }
        _ => Err(AdkError::Artifact("Expected binary data".into())),
    }
}
```

### Version History

```rust
async fn show_history(ctx: &ToolContext, filename: &str) -> Result<Value> {
    let artifacts = ctx.artifacts();
    
    // Get all files
    let files = artifacts.list().await?;
    
    if !files.contains(&filename.to_string()) {
        return Ok(json!({ "error": "File not found" }));
    }
    
    // Note: versions() is not available on the simple Artifacts trait
    // You would need access to the underlying ArtifactService
    
    Ok(json!({
        "file": filename,
        "exists": true
    }))
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
