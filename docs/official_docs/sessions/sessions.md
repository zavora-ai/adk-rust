# Sessions

Sessions in ADK-Rust provide conversation context management, allowing agents to maintain state across multiple interactions. Sessions store conversation history (events) and arbitrary state data that persists throughout a conversation.

## Overview

A session represents a single conversation between a user and an agent. Each session:

- Has a unique identifier
- Belongs to an application (`app_name`) and user (`user_id`)
- Contains a list of events (conversation history)
- Maintains state data (key-value pairs)
- Tracks the last update time

## Session Trait

The `Session` trait defines the interface for session objects:

```rust
use adk_session::{Events, State};
use chrono::{DateTime, Utc};

pub trait Session: Send + Sync {
    /// Unique session identifier
    fn id(&self) -> &str;
    
    /// Application name this session belongs to
    fn app_name(&self) -> &str;
    
    /// User identifier
    fn user_id(&self) -> &str;
    
    /// Access session state
    fn state(&self) -> &dyn State;
    
    /// Access conversation events
    fn events(&self) -> &dyn Events;
    
    /// Last time the session was updated
    fn last_update_time(&self) -> DateTime<Utc>;
}
```

## SessionService Trait

The `SessionService` trait defines operations for managing sessions:

```rust
use adk_session::{CreateRequest, GetRequest, ListRequest, DeleteRequest, Event, Session};
use adk_core::Result;
use async_trait::async_trait;

#[async_trait]
pub trait SessionService: Send + Sync {
    /// Create a new session
    async fn create(&self, req: CreateRequest) -> Result<Box<dyn Session>>;
    
    /// Retrieve an existing session
    async fn get(&self, req: GetRequest) -> Result<Box<dyn Session>>;
    
    /// List all sessions for an app/user
    async fn list(&self, req: ListRequest) -> Result<Vec<Box<dyn Session>>>;
    
    /// Delete a session
    async fn delete(&self, req: DeleteRequest) -> Result<()>;
    
    /// Append an event to a session
    async fn append_event(&self, session_id: &str, event: Event) -> Result<()>;
}
```

## Request Types

### CreateRequest

```rust
use adk_session::CreateRequest;
use std::collections::HashMap;

let request = CreateRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: None,  // Auto-generate UUID if None
    state: HashMap::new(),  // Initial state
};
```

### GetRequest

```rust
use adk_session::GetRequest;

let request = GetRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_abc".to_string(),
    num_recent_events: Some(10),  // Limit events returned
    after: None,  // Filter events after timestamp
};
```

### ListRequest

```rust
use adk_session::ListRequest;

let request = ListRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
};
```

### DeleteRequest

```rust
use adk_session::DeleteRequest;

let request = DeleteRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_abc".to_string(),
};
```

## SessionService Implementations

ADK-Rust provides multiple session service implementations:

| Implementation | Feature Flag | Use Case |
|----------------|-------------|----------|
| `InMemorySessionService` | _(none)_ | Development, testing, single-instance |
| `SqliteSessionService` | `sqlite` | Single-node persistence |
| `PostgresSessionService` | `postgres` | Production relational persistence |
| `RedisSessionService` | `redis` | Low-latency in-memory persistence |
| `MongoSessionService` | `mongodb` | Document-oriented persistence |
| `Neo4jSessionService` | `neo4j` | Graph database persistence |
| `FirestoreSessionService` | `firestore` | Google Cloud Firestore |
| `VertexAiSessionService` | `vertex-session` | Vertex AI Session API |

### InMemorySessionService

Stores sessions in memory. Ideal for development, testing, and single-instance deployments.

```rust
use adk_session::{InMemorySessionService, SessionService, CreateRequest};
use std::collections::HashMap;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Create the service
    let session_service = InMemorySessionService::new();
    
    // Create a session
    let session = session_service.create(CreateRequest {
        app_name: "my_app".to_string(),
        user_id: "user_123".to_string(),
        session_id: None,
        state: HashMap::new(),
    }).await?;
    
    println!("Session ID: {}", session.id());
    println!("App: {}", session.app_name());
    println!("User: {}", session.user_id());
    
    Ok(())
}
```

### SqliteSessionService

Stores sessions in a SQLite database. Suitable for development and single-node deployments requiring persistence.

```rust
use adk_session::{SqliteSessionService, SessionService, CreateRequest};
use std::collections::HashMap;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Connect to database
    let session_service = SqliteSessionService::new("sqlite:sessions.db").await?;
    
    // Run migrations to create tables
    session_service.migrate().await?;
    
    // Create a session
    let session = session_service.create(CreateRequest {
        app_name: "my_app".to_string(),
        user_id: "user_123".to_string(),
        session_id: None,
        state: HashMap::new(),
    }).await?;
    
    println!("Session persisted: {}", session.id());
    
    Ok(())
}
```

> **Note**: The `SqliteSessionService` requires the `sqlite` feature flag:
> ```toml
> adk-session = { version = "0.5.0", features = ["sqlite"] }
> ```

### PostgresSessionService

Stores sessions in PostgreSQL. Suitable for production multi-node deployments.

```rust
use adk_session::{PostgresSessionService, SessionService, CreateRequest};
use std::collections::HashMap;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let session_service = PostgresSessionService::new(
        "postgres://user:pass@localhost:5432/mydb"
    ).await?;
    
    session_service.migrate().await?;
    
    let session = session_service.create(CreateRequest {
        app_name: "my_app".to_string(),
        user_id: "user_123".to_string(),
        session_id: None,
        state: HashMap::new(),
    }).await?;
    
    println!("Session persisted: {}", session.id());
    Ok(())
}
```

> **Note**: Requires the `postgres` feature flag:
> ```toml
> adk-session = { version = "0.5.0", features = ["postgres"] }
> ```

### MongoSessionService

Stores sessions in MongoDB. Supports both standalone and replica-set deployments out of the box.

On startup, `MongoSessionService::new()` auto-detects whether the connected MongoDB instance is part of a replica set by issuing a `hello` command. If a replica set (or sharded cluster) is detected, all multi-document writes use transactions for atomicity. On standalone deployments, operations execute sequentially without transactions — the upsert-based writes are idempotent, so partial failures leave state recoverable.

```rust
use adk_session::{MongoSessionService, SessionService, CreateRequest};
use std::collections::HashMap;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Works with both standalone and replica-set MongoDB
    let session_service = MongoSessionService::new(
        "mongodb://user:pass@localhost:27017",
        "my_sessions_db",
    ).await?;

    // Run migrations (creates indexes)
    session_service.migrate().await?;

    // Check deployment mode
    if session_service.supports_transactions() {
        println!("Replica set detected — transactions enabled");
    } else {
        println!("Standalone mode — sequential writes");
    }

    let session = session_service.create(CreateRequest {
        app_name: "my_app".to_string(),
        user_id: "user_123".to_string(),
        session_id: None,
        state: HashMap::new(),
    }).await?;

    println!("Session persisted: {}", session.id());
    Ok(())
}
```

> **Note**: Requires the `mongodb` feature flag:
> ```toml
> adk-session = { version = "0.5.0", features = ["mongodb"] }
> ```

#### MongoDB deployment modes

| Deployment | Transactions | Behavior |
|------------|-------------|----------|
| Standalone | No | Sequential upserts, idempotent writes |
| Replica Set | Yes | Multi-document ACID transactions |
| Sharded Cluster | Yes | Multi-document ACID transactions |

The `retryWrites=false` connection string parameter is no longer required for standalone deployments. The service handles this transparently.

#### MongoDB collections

`MongoSessionService` uses four collections:

- `sessions` — session documents with session-level state
- `events` — event documents linked by `session_id`
- `app_states` — application-level state keyed by `app_name`
- `user_states` — user-level state keyed by `(app_name, user_id)`

### Neo4jSessionService

Stores sessions as graph nodes in Neo4j. Relationships between sessions, events, and state tiers are modeled as graph edges.

```rust
use adk_session::{Neo4jSessionService, SessionService, CreateRequest};
use std::collections::HashMap;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let session_service = Neo4jSessionService::new(
        "bolt://localhost:7687",
        "neo4j",
        "password",
    ).await?;

    session_service.migrate().await?;

    let session = session_service.create(CreateRequest {
        app_name: "my_app".to_string(),
        user_id: "user_123".to_string(),
        session_id: None,
        state: HashMap::new(),
    }).await?;

    println!("Session persisted: {}", session.id());
    Ok(())
}
```

> **Note**: Requires the `neo4j` feature flag:
> ```toml
> adk-session = { version = "0.5.0", features = ["neo4j"] }
> ```

### RedisSessionService

Stores sessions in Redis. Ideal for low-latency, high-throughput deployments.

```rust
use adk_session::{RedisSessionService, RedisSessionConfig};

let config = RedisSessionConfig::new("redis://localhost:6379");
let session_service = RedisSessionService::new(config).await?;
```

> **Note**: Requires the `redis` feature flag:
> ```toml
> adk-session = { version = "0.5.0", features = ["redis"] }
> ```

## Schema Migrations

All database-backed session services (SQLite, PostgreSQL, MongoDB, Neo4j) include a versioned, forward-only migration system. Migrations are tracked in a `_schema_migrations` registry table.

### Encrypted Sessions

Wrap any `SessionService` with `EncryptedSession` to encrypt session state at rest using AES-256-GCM. Requires the `encrypted-session` feature flag.

```toml
adk-session = { version = "0.5.0", features = ["encrypted-session"] }
```

#### Basic Usage

```rust
use adk_session::{EncryptedSession, EncryptionKey, InMemorySessionService};

// Generate a random 256-bit key
let key = EncryptionKey::generate();

// Or load from environment variable (base64-encoded)
// let key = EncryptionKey::from_env("SESSION_ENCRYPTION_KEY")?;

// Wrap any session service
let inner = InMemorySessionService::new();
let service = EncryptedSession::new(inner, key, vec![]);

// Use exactly like any other SessionService — encryption is transparent
let session = service.create(CreateRequest { /* ... */ }).await?;
```

State is serialized to JSON, encrypted with a random 96-bit nonce, and stored as `[nonce || ciphertext]` in the inner service. Decryption happens transparently on read.

#### Key Rotation

Pass previous keys to support reading data encrypted with older keys:

```rust
let new_key = EncryptionKey::generate();
let old_key = EncryptionKey::from_env("OLD_SESSION_KEY")?;

let service = EncryptedSession::new(inner, new_key, vec![old_key]);
```

On read, the current key is tried first. If decryption fails, each previous key is tried in order. On success with a previous key, the data is automatically re-encrypted with the current key.

#### Key Management

```rust
// Generate random key
let key = EncryptionKey::generate();

// Load from base64-encoded environment variable
let key = EncryptionKey::from_env("MY_KEY")?;

// Create from raw 32 bytes
let key = EncryptionKey::from_bytes(&[0u8; 32])?;

// Access raw bytes (e.g., for storing securely)
let bytes: &[u8; 32] = key.as_bytes();
```

### Running Migrations

Call `migrate()` after constructing any database-backed service. It is idempotent — safe to call on every startup:

```rust
use adk_session::SqliteSessionService;

let service = SqliteSessionService::new("sqlite:sessions.db").await?;
service.migrate().await?;
```

### Checking Schema Version

```rust
let version = service.schema_version().await?;
println!("Current schema version: {version}");
```

### Baseline Detection

If you have an existing database created before the migration system was added, `migrate()` detects the pre-existing tables and registers them as already applied. This avoids destructive re-creation and allows incremental adoption.

### Migration Guarantees

- **Forward-only**: Migrations are applied in order and never rolled back
- **Idempotent**: Running `migrate()` multiple times is safe
- **Checksummed**: Each migration is tracked with a SHA-256 checksum to detect tampering
- **Atomic**: PostgreSQL migrations use advisory locks to prevent concurrent execution

## Session Lifecycle

### 1. Creation

Sessions are created with a `CreateRequest`. If no `session_id` is provided, a UUID is generated automatically.

```rust
use adk_session::{InMemorySessionService, SessionService, CreateRequest};
use std::collections::HashMap;

let service = InMemorySessionService::new();

// Create with auto-generated ID
let session = service.create(CreateRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: None,
    state: HashMap::new(),
}).await?;

// Create with specific ID
let session = service.create(CreateRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: Some("my-custom-id".to_string()),
    state: HashMap::new(),
}).await?;
```

### 2. Retrieval

Retrieve a session by its identifiers:

```rust
use adk_session::GetRequest;

let session = service.get(GetRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_abc".to_string(),
    num_recent_events: None,
    after: None,
}).await?;

println!("Retrieved session: {}", session.id());
println!("Events: {}", session.events().len());
```

### 3. Event Appending

Events are appended to sessions as the conversation progresses. This is typically handled by the Runner, but can be done manually:

```rust
use adk_session::Event;

let event = Event::new("invocation_123");
service.append_event(session.id(), event).await?;
```

### 4. Listing

List all sessions for a user:

```rust
use adk_session::ListRequest;

let sessions = service.list(ListRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
}).await?;

for session in sessions {
    println!("Session: {} (updated: {})", 
        session.id(), 
        session.last_update_time()
    );
}
```

### 5. Deletion

Delete a session when it's no longer needed:

```rust
use adk_session::DeleteRequest;

service.delete(DeleteRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: "session_abc".to_string(),
}).await?;
```

## Using Sessions with Runner

Sessions are typically managed by the `Runner` when executing agents. The Runner:

1. Creates or retrieves a session
2. Passes session context to the agent
3. Appends events as the conversation progresses
4. Updates session state based on agent actions

```rust
use adk_rust::prelude::*;
use adk_rust::{SessionId, UserId};
use adk_runner::{Runner, RunnerConfig};
use adk_session::InMemorySessionService;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);

    let agent = LlmAgentBuilder::new("assistant")
        .model(model)
        .instruction("You are a helpful assistant.")
        .build()?;

    let session_service = Arc::new(InMemorySessionService::new());

    // Create runner with session service
    let runner = Runner::new(RunnerConfig {
        app_name: "my_app".to_string(),
        agent: Arc::new(agent),
        session_service,
        artifact_service: None,
        memory_service: None,
        run_config: None,
    })?;

    // Run with user and session IDs
    let user_content = Content::new("user").with_text("Hello!");
    let stream = runner.run(
        UserId::new("user_123")?,
        SessionId::new("session_abc")?,
        user_content,
    ).await?;

    Ok(())
}
```

## Typed Session Identity

Session operations use `AdkIdentity` — a typed composite of `AppName`, `UserId`, and `SessionId` — to address sessions unambiguously. This eliminates parameter ordering bugs and ensures multi-tenant isolation.

### Typed Session Operations

`SessionService` provides typed methods alongside the existing string-based API:

```rust
use adk_core::{AdkIdentity, AppName, SessionId, UserId};
use adk_session::{AppendEventRequest, Event, SessionService};

let identity = AdkIdentity::new(
    AppName::try_from("my_app")?,
    UserId::try_from("user_123")?,
    SessionId::try_from("session_abc")?,
);

// Typed append — uses the full (app, user, session) triple
let event = Event::new("inv_001");
service.append_event_for_identity(AppendEventRequest {
    identity: identity.clone(),
    event,
}).await?;

// Typed get and delete
let session = service.get_for_identity(&identity, None, None).await?;
service.delete_for_identity(&identity).await?;
```

All session backends (in-memory, SQLite, PostgreSQL, Redis, MongoDB, Firestore, Neo4j, Vertex) support the typed identity path. The legacy `append_event(&str, ...)` method remains available for backward compatibility.

For new code, prefer `append_event_for_identity()` and the other typed identity helpers. The legacy `append_event(&str, ...)` path is retained only for migration and is the first legacy identity API intended for future deprecation once internal callers have fully moved to `AdkIdentity`.

### Multi-Tenant Safety

With typed identity, two sessions that share the same raw `session_id` but differ in `app_name` or `user_id` are always addressed correctly:

```rust
let tenant_a = AdkIdentity::new(
    AppName::try_from("app-a")?,
    UserId::try_from("alice")?,
    SessionId::try_from("shared-session-id")?,
);

let tenant_b = AdkIdentity::new(
    AppName::try_from("app-b")?,
    UserId::try_from("bob")?,
    SessionId::try_from("shared-session-id")?,
);

// These address completely different sessions
assert_ne!(tenant_a, tenant_b);
```

### Reading Identity from Sessions

The `Session` trait provides typed helpers to extract identity from an existing session:

```rust
let session = service.get(get_request).await?;

// Get the full session identity
let identity = session.try_identity()?;

// Or individual typed fields
let app = session.try_app_name()?;
let user = session.try_user_id()?;
let sid = session.try_session_id()?;
```

For more on the three identity layers (auth, session, execution), see [Core Types — Identity](../core/core.md#identity).

## Events

The `Events` trait provides access to conversation history:

```rust
pub trait Events: Send + Sync {
    /// Get all events
    fn all(&self) -> Vec<Event>;
    
    /// Get number of events
    fn len(&self) -> usize;
    
    /// Get event at index
    fn at(&self, index: usize) -> Option<&Event>;
    
    /// Check if empty
    fn is_empty(&self) -> bool;
}
```

Access events from a session:

```rust
let events = session.events();
println!("Total events: {}", events.len());

for event in events.all() {
    println!("Event {} by {} at {}", 
        event.id, 
        event.author, 
        event.timestamp
    );
}
```

## Complete Example

```rust
use adk_session::{
    InMemorySessionService, SessionService, 
    CreateRequest, GetRequest, ListRequest, DeleteRequest,
    Event, KEY_PREFIX_USER,
};
use serde_json::json;
use std::collections::HashMap;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let service = InMemorySessionService::new();
    
    // Create session with initial state
    let mut initial_state = HashMap::new();
    initial_state.insert(format!("{}name", KEY_PREFIX_USER), json!("Alice"));
    initial_state.insert("topic".to_string(), json!("Getting started"));
    
    let session = service.create(CreateRequest {
        app_name: "demo".to_string(),
        user_id: "alice".to_string(),
        session_id: None,
        state: initial_state,
    }).await?;
    
    println!("Created session: {}", session.id());
    
    // Check state
    let state = session.state();
    println!("User name: {:?}", state.get("user:name"));
    println!("Topic: {:?}", state.get("topic"));
    
    // Append an event
    let event = Event::new("inv_001");
    service.append_event(session.id(), event).await?;
    
    // Retrieve session with events
    let session = service.get(GetRequest {
        app_name: "demo".to_string(),
        user_id: "alice".to_string(),
        session_id: session.id().to_string(),
        num_recent_events: None,
        after: None,
    }).await?;
    
    println!("Events: {}", session.events().len());
    
    // List all sessions
    let sessions = service.list(ListRequest {
        app_name: "demo".to_string(),
        user_id: "alice".to_string(),
    }).await?;
    
    println!("Total sessions: {}", sessions.len());
    
    // Delete session
    service.delete(DeleteRequest {
        app_name: "demo".to_string(),
        user_id: "alice".to_string(),
        session_id: session.id().to_string(),
    }).await?;
    
    println!("Session deleted");
    
    Ok(())
}
```

## Related

- [State Management](state.md) - Managing session state with prefixes
- [Context Compaction](context-compaction.md) - Reducing LLM context size
- [Events](../events/events.md) - Event structure and actions
- [Runner](../deployment/launcher.md) - Agent execution with sessions

---

**Previous**: [← MCP Tools](../tools/mcp-tools.md) | **Next**: [State Management →](state.md)
