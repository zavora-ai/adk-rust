# GCS Artifact Service (Roadmap)

> **Status**: Not yet implemented  
> **Priority**: Medium  
> **Est. Effort**: 2-3 weeks

## Overview

`GcsArtifactService` will provide persistent artifact storage using Google Cloud Storage (GCS), enabling artifacts to persist across application restarts and be shared across distributed deployments.

## Planned Features

### Storage Mechanism
- Leverage Google Cloud Storage for durable, scalable artifact persistence
- Each artifact version stored as a separate GCS object (blob)
- Hierarchical object naming: `{app_name}/{user_id}/{session_id}/{filename}/v{version}`

### Key Capabilities
- ✅ **Persistence**: Artifacts survive application restarts
- ✅ **Scalability**: Leverage GCS's proven scalability and durability
- ✅ **Explicit Versioning**: Each version as distinct GCS object
- ✅ **Permissions**: IAM-based access control via Application Default Credentials

## Planned API

### Configuration

```rust,ignore
use adk_artifact::GcsArtifactService;

// Create GCS artifact service
let gcs_service = GcsArtifactService::new(GcsConfig {
    bucket_name: "my-adk-artifacts".to_string(),
    project_id: Some("my-gcp-project".to_string()),
    credentials_path: None,  // Use Application Default Credentials
})?;

// Use in Runner
let runner = Runner::new(RunnerConfig {
    app_name: "my_app".to_string(),
    agent,
    session_service,
    artifact_service: Some(Arc::new(gcs_service)),
    memory_service: None,
})?;
```

### Object Naming Convention

```
Bucket: my-adk-artifacts
Objects:
  ├─ app1/user_123/session_456/report.pdf/v1
  ├─ app1/user_123/session_456/report.pdf/v2
  ├─ app1/user_123/user/settings.json/v1    ← user-scoped (special "user" session)
  └─ app2/user_789/session_999/image.png/v1
```

### Required Permissions

IAM roles needed for the service account:
- `roles/storage.objectCreator` - To create/save artifacts
- `roles/storage.objectViewer` - To load/list artifacts
- `roles/storage.legacyBucketReader` - To list objects in bucket

## Implementation Plan

### Phase 1: Core Storage (Week 1)
- [ ] Add `google-cloud-storage` dependency
- [ ] Create `GcsArtifactService` struct
- [ ] Implement `ArtifactService` trait:
  - [ ] `save()` - Upload to GCS
  - [ ] `load()` - Download from GCS
  - [ ] `list()` - List objects with prefix
  - [ ] `delete()` - Delete GCS objects
  - [ ] `versions()` - List version objects

### Phase 2: Configuration & Auth (Week 2)
- [ ] Support Application Default Credentials
- [ ] Support explicit credentials file path
- [ ] Bucket existence validation on init
- [ ] Permission validation (test write/read)
- [ ] Environment variable configuration

### Phase 3: Optimization & Testing (Week 3)
- [ ] Connection pooling for GCS client
- [ ] Retry logic for transient failures
- [ ] Streaming uploads/downloads for large files
- [ ] Comprehensive integration tests
- [ ] Performance benchmarks

## Example Usage (Planned)

```rust,ignore
use adk_artifact::{GcsArtifactService, SaveRequest};
use adk_core::Part;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize GCS service
    let gcs = GcsArtifactService::new(GcsConfig {
        bucket_name: "my-artifacts".into(),
        project_id: None,  // Auto-detect
        credentials_path: None,  // Use ADC
    })?;

    // Save artifact - persists to GCS
    let report = Part::InlineData {
        data: b"Report content".to_vec(),
        mime_type: "text/plain".into(),
    };

    gcs.save(SaveRequest {
        app_name: "reports".into(),
        user_id: "user_123".into(),
        session_id: "sess_456".into(),
        file_name: "monthly_report.txt".into(),
        part: report,
        version: None,
    }).await?;

    // Data persists even after app restart!
    Ok(())
}
```

## Migration Path

For users moving from `InMemoryArtifactService` to `GcsArtifactService`:

```rust,ignore
// Before (development)
let artifact_service = Arc::new(InMemoryArtifactService::new());

// After (production) - drop-in replacement
let artifact_service = Arc::new(GcsArtifactService::new(GcsConfig {
    bucket_name: env::var("GCS_ARTIFACT_BUCKET")?,
    project_id: None,
    credentials_path: None,
})?);

// No other code changes needed!
```

## Dependencies

- `google-cloud-storage` (or `cloud-storage` crate)
- `google-cloud-auth` for authentication
- `tokio` for async operations

## Alternative Implementations

Following the `ArtifactService` trait, other storage backends could be implemented:
- **S3ArtifactService** - Amazon S3 storage
- **AzureArtifactService** - Azure Blob Storage
- **LocalFileArtifactService** - Local filesystem (for single-server deployments)

---

**Related**:
- [Artifacts Documentation](../official_docs/artifacts/artifacts.md)
- [Gap Analysis](../GAP_ANALYSIS.md) - Missing features overview

**Status**: Ready for implementation once prioritized
