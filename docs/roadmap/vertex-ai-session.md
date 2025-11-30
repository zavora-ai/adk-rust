# VertexAI Session Service (Roadmap)

> **Status**: Not yet implemented  
> **Priority**: High  
> **Est. Effort**: 3-4 weeks

## Overview

The VertexAI Session Service will provide persistent, cloud-based session storage using Google Cloud's Vertex AI platform. This enables session data to persist across application restarts, be shared across distributed deployments, and integrate seamlessly with other Google Cloud services.

## The Problem

The current `InMemorySessionService` and `DatabaseSessionService` have limitations:

- **InMemory**: Sessions lost on restart, not suitable for production
- **Database**: Requires self-managed database infrastructure
- **Scalability**: Manual scaling and maintenance overhead
- **Integration**: Limited integration with Google Cloud ecosystem

## Planned Solution

VertexAI Session Service will leverage Google Cloud's managed infrastructure to provide:

- **Managed Storage**: No database administration required
- **Automatic Scaling**: Handles traffic spikes automatically
- **High Availability**: Built-in redundancy and failover
- **Cloud Integration**: Native integration with Vertex AI, Cloud Storage, and other GCP services
- **Security**: IAM-based access control and encryption at rest

## Planned Architecture

### Service Configuration

```rust,ignore
use adk_session::VertexAISessionService;

// Create VertexAI session service
let session_service = VertexAISessionService::new(VertexAIConfig {
    project_id: "my-gcp-project".to_string(),
    location: "us-central1".to_string(),
    credentials_path: None,  // Use Application Default Credentials
})?;

// Use in Runner
let runner = Runner::new(RunnerConfig {
    app_name: "my_app".to_string(),
    agent,
    session_service: Arc::new(session_service),
    artifact_service: None,
    memory_service: None,
})?;
```

### Data Model

Sessions will be stored in Vertex AI's managed storage with the following structure:

```
Project: my-gcp-project
Location: us-central1
Sessions:
  ├─ app1/user_123/session_456
  │   ├─ metadata: { app_name, user_id, session_id, created_at, updated_at }
  │   ├─ state: { key-value pairs }
  │   └─ events: [ event1, event2, ... ]
  ├─ app1/user_123/session_789
  └─ app2/user_456/session_abc
```

### Session Storage

Each session contains:
- **Metadata**: App name, user ID, session ID, timestamps
- **State**: Key-value state data with prefix scoping (app:, user:, temp:)
- **Events**: Chronological list of conversation events
- **Artifacts**: References to artifact storage (GCS)

## Implementation Plan

### Phase 1: Core Integration (Week 1-2)
- [ ] Add Vertex AI SDK dependencies
- [ ] Create `VertexAISessionService` struct
- [ ] Implement `SessionService` trait:
  - [ ] `create()` - Create new session
  - [ ] `get()` - Retrieve session by ID
  - [ ] `list()` - List sessions for user
  - [ ] `delete()` - Delete session
  - [ ] `append_event()` - Add event to session

### Phase 2: State Management (Week 2-3)
- [ ] Implement state operations:
  - [ ] `get_state()` - Retrieve state values
  - [ ] `set_state()` - Update state values
  - [ ] `delete_state()` - Remove state keys
- [ ] Support state prefixes (app:, user:, temp:)
- [ ] Implement state delta tracking
- [ ] Add state synchronization

### Phase 3: Authentication & Configuration (Week 3)
- [ ] Support Application Default Credentials
- [ ] Support explicit credentials file
- [ ] Support service account impersonation
- [ ] Environment variable configuration
- [ ] Connection pooling and retry logic

### Phase 4: Testing & Documentation (Week 4)
- [ ] Integration tests with Vertex AI
- [ ] Performance benchmarks
- [ ] Migration guide from other session services
- [ ] Example applications
- [ ] Best practices documentation

## Example Usage (Planned)

### Basic Setup

```rust,ignore
use adk_session::{VertexAISessionService, VertexAIConfig};
use adk_runner::{Runner, RunnerConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize VertexAI session service
    let session_service = VertexAISessionService::new(VertexAIConfig {
        project_id: env::var("GCP_PROJECT_ID")?,
        location: "us-central1".to_string(),
        credentials_path: None,  // Use ADC
    })?;

    // Create runner with VertexAI sessions
    let runner = Runner::new(RunnerConfig {
        app_name: "my_agent".to_string(),
        agent: my_agent,
        session_service: Arc::new(session_service),
        artifact_service: None,
        memory_service: None,
    })?;

    // Sessions now persist in Vertex AI!
    Ok(())
}
```

### Session Operations

```rust,ignore
// Create a new session
let session = session_service.create(ctx, &CreateRequest {
    app_name: "chatbot".to_string(),
    user_id: "user_123".to_string(),
    state: HashMap::from([
        ("user:name".to_string(), json!("Alice")),
        ("user:preferences".to_string(), json!({"theme": "dark"})),
    ]),
}).await?;

// Retrieve existing session
let session = session_service.get(ctx, &GetRequest {
    app_name: "chatbot".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_456".to_string(),
}).await?;

// List user's sessions
let sessions = session_service.list(ctx, &ListRequest {
    app_name: "chatbot".to_string(),
    user_id: "user_123".to_string(),
}).await?;

// Append event to session
session_service.append_event(ctx, &AppendEventRequest {
    app_name: "chatbot".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_456".to_string(),
    event: new_event,
}).await?;
```

### State Management

```rust,ignore
// Update session state
session_service.set_state(ctx, &SetStateRequest {
    app_name: "chatbot".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_456".to_string(),
    key: "temp:current_topic".to_string(),
    value: json!("weather"),
}).await?;

// Retrieve state
let value = session_service.get_state(ctx, &GetStateRequest {
    app_name: "chatbot".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_456".to_string(),
    key: "temp:current_topic".to_string(),
}).await?;
```

## Migration Path

### From InMemorySessionService

```rust,ignore
// Before (development)
let session_service = Arc::new(InMemorySessionService::new());

// After (production) - drop-in replacement
let session_service = Arc::new(VertexAISessionService::new(VertexAIConfig {
    project_id: env::var("GCP_PROJECT_ID")?,
    location: "us-central1".to_string(),
    credentials_path: None,
})?);

// No other code changes needed!
```

### From DatabaseSessionService

```rust,ignore
// Migration script to export from database and import to Vertex AI
use adk_session::{DatabaseSessionService, VertexAISessionService};

async fn migrate_sessions(
    db_service: &DatabaseSessionService,
    vertex_service: &VertexAISessionService,
) -> Result<()> {
    // List all sessions from database
    let sessions = db_service.list_all(ctx).await?;
    
    // Import each session to Vertex AI
    for session in sessions {
        vertex_service.import_session(ctx, session).await?;
    }
    
    Ok(())
}
```

## Configuration Options

### Environment Variables

```bash
# Required
export GCP_PROJECT_ID="my-project"
export GCP_LOCATION="us-central1"

# Optional
export GOOGLE_APPLICATION_CREDENTIALS="/path/to/credentials.json"
export VERTEX_AI_SESSION_TIMEOUT="30s"
export VERTEX_AI_MAX_RETRIES="3"
```

### Programmatic Configuration

```rust,ignore
let config = VertexAIConfig {
    project_id: "my-project".to_string(),
    location: "us-central1".to_string(),
    credentials_path: Some("/path/to/creds.json".to_string()),
    timeout: Duration::from_secs(30),
    max_retries: 3,
    enable_compression: true,
};

let service = VertexAISessionService::new(config)?;
```

## Required Permissions

IAM roles needed for the service account:

- `roles/aiplatform.user` - Access Vertex AI services
- `roles/storage.objectViewer` - Read session data
- `roles/storage.objectCreator` - Write session data

Minimal custom role:
```json
{
  "title": "ADK Session Manager",
  "description": "Manage ADK sessions in Vertex AI",
  "includedPermissions": [
    "aiplatform.sessions.create",
    "aiplatform.sessions.get",
    "aiplatform.sessions.list",
    "aiplatform.sessions.update",
    "aiplatform.sessions.delete"
  ]
}
```

## Performance Considerations

### Caching

```rust,ignore
// Enable local caching for frequently accessed sessions
let service = VertexAISessionService::builder()
    .project_id("my-project")
    .location("us-central1")
    .enable_cache(true)
    .cache_ttl(Duration::from_secs(300))
    .build()?;
```

### Batch Operations

```rust,ignore
// Batch append events for better performance
let events = vec![event1, event2, event3];
session_service.append_events_batch(ctx, &AppendEventsBatchRequest {
    app_name: "chatbot".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_456".to_string(),
    events,
}).await?;
```

### Connection Pooling

```rust,ignore
// Configure connection pool
let service = VertexAISessionService::builder()
    .project_id("my-project")
    .location("us-central1")
    .max_connections(50)
    .connection_timeout(Duration::from_secs(10))
    .build()?;
```

## Comparison with adk-go

ADK-Go has VertexAI session support with:
- Managed session storage in Vertex AI
- Automatic scaling and high availability
- IAM-based access control
- Integration with other GCP services
- Production-ready deployment patterns

ADK-Rust will achieve feature parity with these capabilities.

## Dependencies

- `google-cloud-aiplatform` - Vertex AI SDK
- `google-cloud-auth` - Authentication
- `tokio` - Async runtime
- `serde` - Serialization

## Related Features

### Artifact Integration

VertexAI sessions can reference artifacts stored in GCS:

```rust,ignore
// Session references artifact in GCS
let event = Event {
    content: Some(Content {
        parts: vec![
            Part::Text("Here's the report".to_string()),
            Part::FileData {
                file_uri: "gs://my-bucket/artifacts/report.pdf".to_string(),
                mime_type: "application/pdf".to_string(),
            },
        ],
    }),
    // ...
};
```

### Telemetry Integration

Automatic telemetry for session operations:

```rust,ignore
// Spans automatically created for:
// - session.create
// - session.get
// - session.append_event
// - session.set_state
```

## Best Practices

### Session Lifecycle

1. **Create**: Create sessions on first user interaction
2. **Reuse**: Reuse sessions for conversation continuity
3. **Clean Up**: Delete old sessions periodically
4. **Monitor**: Track session metrics and errors

### State Management

1. **Prefix Keys**: Use app:, user:, temp: prefixes appropriately
2. **Minimize State**: Store only essential data
3. **Validate**: Validate state data before storage
4. **Version**: Version state schemas for compatibility

### Error Handling

```rust,ignore
match session_service.get(ctx, &request).await {
    Ok(session) => process_session(session),
    Err(AdkError::SessionNotFound(_)) => create_new_session(),
    Err(AdkError::PermissionDenied(_)) => handle_auth_error(),
    Err(e) => handle_other_error(e),
}
```

## Timeline

VertexAI Session Service is planned for a future release. The implementation will follow the design patterns established in ADK-Go while leveraging Rust's type safety and async capabilities.

Key milestones:
1. Core Vertex AI SDK integration
2. SessionService trait implementation
3. State management and synchronization
4. Authentication and configuration
5. Performance optimization
6. Comprehensive testing and documentation

## Contributing

If you're interested in contributing to VertexAI session support in ADK-Rust, please:

1. Review the existing code in `adk-session/`
2. Familiarize yourself with Vertex AI APIs
3. Check the ADK-Go implementation for reference
4. Open an issue to discuss your approach

---

**Related**:
- [Sessions Documentation](../official_docs/sessions/sessions.md)
- [State Management Documentation](../official_docs/sessions/state.md)
- [GCS Artifacts Roadmap](./gcs-artifacts.md)

**Note**: This is a roadmap document. The APIs and examples shown here are illustrative and subject to change during implementation.
