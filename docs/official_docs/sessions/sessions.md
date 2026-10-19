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
use adk_core::{AdkIdentity, Result};
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

    /// Rewind a session to an event, or by a number of events (default: Unsupported)
    async fn rewind(&self, identity: &AdkIdentity, target_event_id: &str) -> Result<Box<dyn Session>>;
    async fn rewind_steps(&self, identity: &AdkIdentity, steps: usize) -> Result<Box<dyn Session>>;
}
```

The trait also has identity-addressed methods (`get_for_identity`, `append_event_for_identity`,
`delete_for_identity`), `delete_all_sessions`, and `health_check`; see
[Typed Session Identity](#typed-session-identity).

### Behaviour Every Backend Follows

| Operation | Behaviour |
|-----------|-----------|
| `create` with an existing `(app_name, user_id, session_id)` | Fails with `session.already_exists` (`Conflict`, HTTP 409) and writes nothing: the existing session keeps its events and state, and the app and user tiers are untouched |
| `get` of a missing session | Fails with `session.not_found` (`NotFound`, HTTP 404); a backend failure is never reported as not found |
| `append_event`, `append_event_for_identity` to a missing session | Fails with `session.not_found` |
| Event order | Timestamp order; events with equal timestamps keep their append order |
| `list` | Most recently updated first; equal update times by session ID, descending, so `offset`/`limit` pages are stable |
| `rewind`, `rewind_steps` | Change only the session the identity names, and rebuild its state from the state it was created with plus the remaining events' deltas; app and user state are not rewound |

Strings containing U+0000 in event content and state read back unchanged. PostgreSQL stores
them in an escaped form, since `JSONB` cannot hold U+0000; strings without U+0000 are stored as
they are.

Rewind is implemented by `InMemorySessionService`, `SqliteSessionService`, and
`EncryptedSession` over either; the other backends return `session.rewind.unsupported`.

```rust
use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use adk_session::{InMemorySessionService, SessionService};

async fn drop_last_turn(service: &InMemorySessionService) -> adk_core::Result<()> {
    let identity = AdkIdentity::new(
        AppName::try_from("my_app")?,
        UserId::try_from("user_123")?,
        SessionId::try_from("session_abc")?,
    );
    // The last turn is the user message and the model's reply.
    let session = service.rewind_steps(&identity, 2).await?;
    println!("{} events remain", session.events().len());
    Ok(())
}
```

### Get or Create

Create a session only when `get` reports it missing, and read it again when `create` loses a
race to a concurrent request. Creating on any `get` error turns one transient backend failure
into an attempt to replace the session.

```rust
use adk_session::{CreateRequest, GetRequest, Session, SessionService};
use std::collections::HashMap;

async fn get_or_create(
    service: &dyn SessionService,
    app_name: &str,
    user_id: &str,
    session_id: &str,
) -> adk_core::Result<Box<dyn Session>> {
    let get = || GetRequest {
        app_name: app_name.to_string(),
        user_id: user_id.to_string(),
        session_id: session_id.to_string(),
        num_recent_events: None,
        after: None,
    };
    match service.get(get()).await {
        Err(error) if error.is_not_found() => {
            let created = service
                .create(CreateRequest {
                    app_name: app_name.to_string(),
                    user_id: user_id.to_string(),
                    session_id: Some(session_id.to_string()),
                    state: HashMap::new(),
                })
                .await;
            match created {
                Err(error) if error.is_conflict() => service.get(get()).await,
                other => other,
            }
        }
        other => other,
    }
}
```

`adk-server`'s `/api/run` and `/api/run_sse`, both A2A executors, the background worker, and
the runner's externally triggered invocations follow this pattern.

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
    limit: Some(20), // None returns every session
    offset: None,    // Sessions to skip, for paging
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

### Identifier Validation

Every backend validates the identifiers a call carries before it reads or writes storage, and
rejects an invalid one with `session.invalid_identifier` (`ErrorCategory::InvalidInput`,
HTTP 400). An identifier is valid when it is non-empty, at most `MAX_ID_LEN` (512) bytes, and
free of NUL characters — the `AppName`, `UserId`, and `SessionId` rules. `Runner::run_str`
and every conversion of an `IdentityError` into `AdkError` return the same code. A backend
with stricter rules of its own reports them under its own code after these checks pass, such
as `session.firestore.invalid_identifier` for an ID Firestore cannot use as a document ID.

| Method | Validated identifiers |
|--------|-----------------------|
| `create` | `app_name`, `user_id`, and `session_id` when one is given |
| `get`, `delete` | `app_name`, `user_id`, `session_id` |
| `list`, `delete_all_sessions` | `app_name`, `user_id` |
| `append_event` | `session_id` |
| `append_event_for_identity`, `get_for_identity`, `delete_for_identity` | Validated when the `AdkIdentity` is built |

`CreateRequest::try_identity()` validates `app_name` and `user_id` even when `session_id` is
`None`, so a custom backend can validate a create request with one call.

## SessionService Implementations

### Choosing a Backend

```mermaid
flowchart TD
    Start["Choose Session Backend"] --> Q1{"Need persistence?"}
    Q1 -->|No| InMemory["InMemorySessionService<br/>Fast, ephemeral"]
    Q1 -->|Yes| Q2{"Scale requirements?"}
    
    Q2 -->|"Single server"| SQLite["SQLite<br/>File-based, zero config"]
    Q2 -->|"Multi-server"| Q3{"Latency priority?"}
    
    Q3 -->|"Sub-ms reads"| Redis["Redis<br/>In-memory, TTL support"]
    Q3 -->|"Durability"| Q4{"Existing infra?"}
    
    Q4 -->|PostgreSQL| Postgres["PostgreSQL<br/>ACID, JSONB state"]
    Q4 -->|MongoDB| Mongo["MongoDB<br/>Document-native"]
    Q4 -->|Google Cloud| Firestore["Firestore<br/>Serverless, auto-scale"]
    Q4 -->|Graph queries| Neo4j["Neo4j<br/>Relationship-rich state"]
```

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

`vertex-session` is also forwarded by the `adk-rust` umbrella crate:

```toml
[dependencies]
adk-rust = { version = "3.0.0", features = ["vertex-session"] }
```

### VertexAiSessionService

`VertexAiSessionService` persists sessions through the GA `v1` Vertex AI Agent
Engine Session API. It uses Application Default Credentials and accepts either
a numeric reasoning-engine ID or its complete resource name:

```rust
use adk_session::{VertexAiSessionConfig, VertexAiSessionService};

let config = VertexAiSessionConfig::new("my-project", "us-central1")
    .with_reasoning_engine("1234567890");
let service = VertexAiSessionService::new_with_adc(config)?;
```

Inside a deployed Vertex AI Agent Engine container the platform provides the
configuration as environment variables, and `VertexAiSessionConfig::from_env()`
reads them:

| Variable | Meaning |
|----------|---------|
| `GOOGLE_CLOUD_PROJECT` | Google Cloud project ID |
| `GOOGLE_CLOUD_LOCATION` | GCP region |
| `GOOGLE_CLOUD_AGENT_ENGINE_ID` | Bare numeric agent engine ID (not a full resource name) |

```rust
use adk_session::{VertexAiSessionConfig, VertexAiSessionService};

let config = VertexAiSessionConfig::from_env()?;
let service = VertexAiSessionService::new_with_adc(config)?;
```

Missing or blank variables produce an invalid-input error naming each one.

#### Session expiration

`ttl` and `expireTime` are the `Session.expiration` oneof members
(`google/cloud/aiplatform/v1beta1/session.proto`). The config sends at most
one on session create: `ttl` is input-only, serialized as a JSON duration
string (e.g. `"86400s"`), with a 24-hour minimum; `expireTime` is an RFC 3339
timestamp. Setting both members, or a TTL below 24 hours, fails at service
construction.

```rust
use adk_session::VertexAiSessionConfig;
use std::time::Duration;

let config = VertexAiSessionConfig::new("my-project", "us-central1")
    .with_reasoning_engine("1234567890")
    .with_ttl(Duration::from_secs(86_400)); // sent as "86400s"
```

#### Identity isolation

The public session ID is always the ID supplied to `CreateRequest`, or a locally
generated UUID when no ID is supplied. The backend derives the Vertex resource
ID from the complete `(app_name, user_id, session_id)` tuple and persists a
versioned identity marker in session state.

| Property | Behavior |
|----------|----------|
| Logical ID | Preserved across create, get, list, append, and delete |
| Remote ID | Deterministic 63-character `adk1-…` ID derived from the complete identity |
| Identity marker | Stored as protected state and removed from public session state |
| Shared reasoning engine | Sessions with the same logical ID remain isolated by app and user |
| Reserved state | Create state and event state deltas cannot set the identity marker |

The backend verifies both the marker and the computed resource ID before every
identity-scoped operation. A missing or mismatched marker on a computed resource
is treated as corruption rather than as another tenant's session.

#### Legacy session migration

Python ADK and pre-v2 sessions may use their logical ID directly and omit the
identity marker. Compatibility depends on how the reasoning-engine parent is
selected:

| Configuration | Unmarked-session behavior |
|---------------|---------------------------|
| No fixed reasoning engine | Enabled only when `app_name` is the canonical nonzero numeric reasoning-engine ID without leading zeros |
| Fixed reasoning engine | Disabled by default |
| Fixed engine plus `allow_unmarked_sessions_for_app("app")` | Enabled only for that app |

```rust
use adk_session::{VertexAiSessionConfig, VertexAiSessionService};

let config = VertexAiSessionConfig::new("my-project", "us-central1")
    .with_reasoning_engine("1234567890");
let service = VertexAiSessionService::new_with_adc(config)?
    .allow_unmarked_sessions_for_app("legacy-app");
```

> **Important:** An unmarked session does not prove app ownership. Enable the
> fixed-engine compatibility option only when the named app exclusively owns
> the legacy sessions in that reasoning engine. Exact `user_id` matching still
> applies. Marked-direct resources, the reserved `adk1-` namespace, and
> computed/direct ambiguities fail closed.

The legacy `append_event(session_id, event)` API also requires exactly one
cached app/user scope. Call create, get, or list first, or use
`append_event_for_identity()` directly. The cache is bounded, so a long-running
process must get or list an older session again after its scope is evicted.

#### Endpoints and event fidelity

| Location | Default endpoint |
|----------|------------------|
| `global` | `https://aiplatform.googleapis.com` |
| `us` | `https://aiplatform.us.rep.googleapis.com` |
| `eu` | `https://aiplatform.eu.rep.googleapis.com` |
| Regional location | `https://LOCATION-aiplatform.googleapis.com` |

Custom endpoints must be trusted origins because they receive Google
authorization headers and complete session/event payloads. Construction fails
unless the endpoint is an HTTPS origin without userinfo, a path, query, or
fragment; loopback HTTP is allowed for tests. Redirects are disabled.

#### Production bounds

| Boundary | Default |
|----------|---------|
| Vertex `user_id` | At most 128 Unicode scalar values |
| Connect deadline | 10 seconds |
| Credential-header deadline | 30 seconds |
| HTTP request deadline | 120 seconds |
| Long-running-operation polling | 120 seconds |
| Encoded request body | 64 MiB per create/append request |
| Decoded response body | 64 MiB per response and in aggregate across one paginated list operation |
| Pagination elapsed time | 120 seconds for the complete list operation |
| Page token | 64 KiB |
| JSON and Vertex Struct nesting | 64 levels |

`with_max_request_bytes()` changes the outbound body budget,
`with_max_response_bytes()` changes both inbound body budgets, and
`with_pagination_timeout()` changes the total list deadline. A high body limit
weakens the default allocation protection; these limits apply independently to
each request, response, or list operation rather than globally across
concurrent calls. They are byte budgets, not total process-memory ceilings
after JSON parsing and base64 decoding.
`with_credentials()` is fallible because it validates the endpoint and builds
the bounded, redirect-disabled HTTP client.

Recent-event limits and timestamp lower bounds are pushed into `events.list`.
The backend requests descending pages only until the recent-event bound is
satisfied, then restores ascending chronological order. `num_recent_events = 0`
does not call the event-list endpoint.

Create, delete, and append HTTP `408`/`5xx`, transport/body, malformed `2xx`,
or invalid successful-result failures can occur after the service commits the
mutation. They return
`session.vertex.{create,delete,append}_outcome_ambiguous` with
`retry.should_retry = false`. After create or delete returns an operation name,
poll transport/status failures and malformed or invalid successful responses
use the same non-retryable ambiguity contract and include the operation name.
Inspect the target session, operation, or event list before any manual retry.
A terminal `done: true` operation error is a known failed result instead: it
retains `session.vertex.operation_failed` and its category-derived retry hint.

Direct HTTP `4xx` rejections other than `408` remain definitive; `429` is
retryable. Any poll failure after an operation is accepted, including `429`, is
ambiguous and non-retryable. The backend does not throttle or retry internally;
use the current [Vertex AI
quotas](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/quotas)
to size application-level concurrency and retry policy.

Run the ignored GA `v1` canary only against an existing reasoning engine with
Application Default Credentials configured. It creates and deletes one session
and one representative event; it never creates or deletes the engine.

```bash
ADK_VERTEX_LIVE_TEST=1 \
GOOGLE_CLOUD_PROJECT=PROJECT_ID \
GOOGLE_CLOUD_LOCATION=LOCATION \
GOOGLE_CLOUD_REASONING_ENGINE_ID=REASONING_ENGINE_ID \
cargo test -p adk-session --features vertex-session \
  --test session_contract_vertex test_vertex_live_ga_v1_canary \
  -- --ignored --exact --nocapture
```

Canonical Vertex content, Google ADK `rawEvent` fields, and arbitrary
`rawEvent` Struct values round-trip without silent data loss. Arbitrary Struct
values remain opaque: reappend preserves every original key and value and adds
the reserved `_adkRust` envelope, so removing that envelope yields the original
Struct. A pre-existing malformed `_adkRust` key fails closed. Google ADK
projection is best-effort even when a Struct has a non-empty string `id`,
numeric `timestamp`, and string `invocationId` and `author`; incompatible
content, actions, metadata, or optional fields do not reject the canonical
SessionEvent.

The GA `v1` `FunctionCall` and `FunctionResponse` wire messages have no `id`.
ID-bearing ADK function parts and empty or noncanonical Base64 thought-signature
bytes use the lossless private `rawEvent` path. `Part.mediaResolution` accepts a
bounded JSON object on any otherwise-valid canonical part. Top-level
`inlineData`/`fileData` `displayName` values are accepted from the deployed GA
wire. ADK projection omits these provider-only fields, while the canonical
sidecar preserves them across reappend. Private-envelope validation accepts
integer or Struct-normalized `1.0` schema versions, treats omitted proto3
default scalar fields as equivalent to their empty private values, compares
safe integer/double-normalized Vertex Struct values semantically, and restores
exact private scalar presence.

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
> adk-session = { version = "3.0.0", features = ["sqlite"] }
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
> adk-session = { version = "3.0.0", features = ["postgres"] }
> ```

### MongoSessionService

Stores sessions in MongoDB. Supports both standalone and replica-set deployments out of the box.

On startup, `MongoSessionService::new()` issues a `hello` command and enables transactions when the reply names a replica set (`setName`) or comes from a sharded cluster's `mongos` router (`msg: "isdbgrid"`). Multi-document writes then run in one transaction; otherwise they execute one after another without a transaction.

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
> adk-session = { version = "3.0.0", features = ["mongodb"] }
> ```

#### MongoDB deployment modes

| Deployment | `hello` reply | Transactions | Behavior |
|------------|---------------|--------------|----------|
| Standalone | Neither `setName` nor `msg: "isdbgrid"` | No | Sequential writes |
| Replica set | `setName` | Yes | Multi-document transactions |
| Sharded cluster (`mongos`) | `msg: "isdbgrid"` | Yes | Multi-document transactions |

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
> adk-session = { version = "3.0.0", features = ["neo4j"] }
> ```

### RedisSessionService

Stores sessions in Redis. Ideal for low-latency, high-throughput deployments.

```rust
use adk_session::{RedisSessionConfig, RedisSessionService};

// Standalone server (or Sentinel)
let session_service = RedisSessionService::new(RedisSessionConfig {
    url: "redis://localhost:6379".to_string(),
    ttl: None,
    cluster_nodes: None,
})
.await?;

// Redis Cluster: seed nodes as host:port
let cluster_service = RedisSessionService::new(RedisSessionConfig {
    url: "redis-cluster://10.0.0.1:6379".to_string(),
    ttl: None,
    cluster_nodes: Some(vec!["10.0.0.1:6379".to_string(), "10.0.0.2:6379".to_string()]),
})
.await?;
```

> **Note**: Requires the `redis` feature flag:
> ```toml
> adk-session = { version = "3.0.0", features = ["redis"] }
> ```

The service picks its key layout from the connection. A standalone server keeps the key names
earlier releases wrote, so existing sessions stay readable. A cluster connection (`cluster_nodes`,
or a `redis-cluster://` URL) puts each session's hash and event set under one hash tag, with `{`
and `}` in identifiers percent-encoded so no identifier can move a key's slot:

| Data | Standalone key | Cluster key |
|------|----------------|-------------|
| Session hash | `<app>:<user>:<session>` | `adk:session:{<app>:<user>:<session>}` |
| Events | `<app>:<user>:<session>:events` | `adk:session:{<app>:<user>:<session>}:events` |
| App state | `app_state:<app>` | `adk:app_state:{<app>}` |
| User state | `user_state:<app>:<user>` | `adk:user_state:{<app>:<user>}` |
| Session index | `sessions_idx:<app>:<user>` | `adk:sessions_idx:{<app>:<user>}` |
| Session index by update time | `sessions_idx:<app>:<user>:updated` | `adk:sessions_idx:{<app>:<user>}:updated` |
| Session lookup | `session_lookup:<session>` | `adk:session_lookup:{<session>}` |

Each event is scored one above the highest score in its set, so events keep their append order,
and `get` then orders them by timestamp. Events written by earlier releases are scored by their
millisecond timestamp and keep their order; events appended to such a session sort after them.
`list` pages through the update-time index, newest first. Sessions created by earlier releases
are only in the set index; the first `list` of their user adds them to the update-time index.

Moving existing data from a standalone server to a cluster means copying its keys to the
cluster names.

| Operation | Standalone | Cluster |
|-----------|------------|---------|
| Append | One Lua script (`EVAL`) writes the event, the update-time index entry, and the session, user, and app state fields | One script writes the event and the session tier; the update-time index entry and the user and app tier fields follow, one command each |
| Create | One script, which writes nothing when the session exists | A check that the session is absent; index entries, lookup, and tier fields; the session hash last, in a script that writes nothing when it exists |
| Delete | One `MULTI`/`EXEC` | Session hash and events in one `DEL`; then the lookup and the index entries |

Redis Cluster runs a script or transaction only when all its keys share one slot, and the app
and user tiers are shared by many sessions, so in cluster mode:

- A reader can see an appended event before its app and user tier fields.
- An append that fails after its script returns an error with the event already stored.
- A create that fails part-way can leave its app and user tier fields, and index entries and a
  lookup for a session that does not exist; `list` skips and removes them, and `get` reports the
  session as not found.
- Of two creates racing for one identity, the one that loses fails with `session.already_exists`
  after writing its app and user tier fields.

The session tier keeps its compare-and-swap in both layouts, and tier fields are written one
field at a time, so concurrent writers of different fields keep each other's values. The server
must allow scripting.

### FirestoreSessionService

Stores sessions in Google Cloud Firestore using Application Default Credentials.

```rust
use adk_session::{FirestoreSessionConfig, FirestoreSessionService};

let config = FirestoreSessionConfig {
    project_id: "my-gcp-project".to_string(),
    root_collection: None, // defaults to "adk_sessions"
};
let session_service = FirestoreSessionService::new(config).await?;
```

| Data | Document path |
|------|---------------|
| Session | `{root}/{app_name}/sessions/{session_id}` |
| Event | `{root}/{app_name}/sessions/{session_id}/events/{event_id}` |
| App state | `{root}/{app_name}/app_state/current` |
| User state | `{root}/{app_name}/users/{user_id}/state/current` |

A session ID is unique within an app, and the owning `user_id` is a field of the session document. Every write re-reads that document inside its transaction and applies only when the app, user, and session ID match the request:

| Operation | Session missing, or owned by another user |
|-----------|-------------------------------------------|
| `create` | Fails with `session.already_exists` (`Conflict`) when the ID is taken in the app |
| `get`, `append_event_for_identity` | Fails with `session.not_found` |
| `delete` | No-op, identical to deleting a missing session |
| `list`, `delete_all_sessions` | Only the caller's own sessions are returned or removed |

A new session never inherits events left under its path. App names, user IDs, and session IDs are document IDs, so they must not contain `/`.

> **Note**: Requires the `firestore` feature flag:
> ```toml
> adk-session = { version = "3.0.0", features = ["firestore"] }
> ```

## Schema Migrations

All database-backed session services (SQLite, PostgreSQL, MongoDB, Neo4j) include a versioned, forward-only migration system. Applied versions are recorded in the `_adk_session_migrations` table (SQLite, PostgreSQL) or collection (MongoDB), or as `_AdkSessionMigration` nodes (Neo4j).

| Backend | Version 2 |
|---------|-----------|
| SQLite | Adds `sessions.initial_state`, the state a session was created with, which rewind rebuilds from. Sessions created before it rewind to their keys that no event has written |
| PostgreSQL | Adds `events.seq`, an insertion sequence that orders events with equal timestamps; existing rows are numbered in their physical order |
| MongoDB | Adds the `idx_events_read_order` index on the event read order (timestamp, then `seq`) |

### Encrypted Sessions

Wrap any `SessionService` with `EncryptedSession` to encrypt session state and event payloads at rest using AES-256-GCM. Requires the `encrypted-session` feature flag.

```toml
adk-session = { version = "3.0.0", features = ["encrypted-session"] }
```

#### Basic Usage

```rust
use adk_session::{
    CreateRequest, EncryptedSession, EncryptionKey, InMemorySessionService, SessionService,
};
use std::collections::HashMap;

// Generate a random 256-bit key
let key = EncryptionKey::generate();

// Or load from environment variable (base64-encoded)
// let key = EncryptionKey::from_env("SESSION_ENCRYPTION_KEY")?;

// Wrap any session service
let inner = InMemorySessionService::new();
let service = EncryptedSession::new(inner, key, vec![]);

// Use exactly like any other SessionService — encryption is transparent
let session = service
    .create(CreateRequest {
        app_name: "my_app".to_string(),
        user_id: "user_123".to_string(),
        session_id: None,
        state: HashMap::new(),
    })
    .await?;
```

Each value is serialized to JSON, encrypted with a random 96-bit nonce, and stored as an `adk-enc:v1:` envelope (base64 of `[nonce || ciphertext]`). Decryption happens transparently on `create`, `get`, and `list`.

| Data | Stored in the inner service as |
|------|--------------------------------|
| State values (create state and event `state_delta`) | Encrypted per value; key names stay in plaintext |
| Event content, LLM response metadata, actions, long-running tool IDs | One encrypted envelope in the event content |
| Event ID, timestamp, invocation ID, branch, author | Plaintext, so the inner service can order and filter events |
| `llm_request`, event `provider_metadata` | Not persisted |

- **Identity binding**: the associated data of each ciphertext names its app, user, session, and state key or event ID. A value or event copied to another session fails with `session.encryption.decrypt_failed`.
- **State tiers**: `app:` and `user:` values are stored encrypted in the shared app and user tiers, so they stay visible to every session of that app or user. `temp:` keys are dropped before encryption.
- **Addressing**: `append_event(session_id, ..)` returns `session.encryption.identity_required` because a bare session ID does not identify the ciphertext binding. Use `append_event_for_identity()`, which the runner uses.

#### Key Rotation

Pass previous keys to support reading data encrypted with older keys:

```rust
use adk_session::{EncryptedSession, EncryptionKey};

let new_key = EncryptionKey::generate();
let old_key = EncryptionKey::from_env("OLD_SESSION_KEY")?;

let service = EncryptedSession::new(inner, new_key, vec![old_key]);
```

On read, the current key is tried first, then each previous key in order. When `get` finds state values under a previous key, it re-encrypts them with the current key by appending a state-only event (hidden from returned events) and returns any failure.

> **Note:** `SessionService` cannot rewrite stored events, so events written under a previous key stay readable only while that key remains in `previous_keys`. Keep a retired key until the sessions that used it are deleted or expire. In a rolling deployment, first add the new key to `previous_keys` everywhere, then make it the current key.

#### Migrating Data From Earlier Releases

Releases before the per-value format stored `create` state as one `__encrypted_state` blob and passed event content and state deltas through in plaintext. Reads reject such data with `session.encryption.unencrypted_data`. Enable migration while existing sessions are converted:

```rust
use adk_session::{EncryptedSession, EncryptionKey, InMemorySessionService};

let key = EncryptionKey::from_env("SESSION_ENCRYPTION_KEY")?;
let service = EncryptedSession::new(InMemorySessionService::new(), key, vec![])
    .with_legacy_migration(true);
```

`get` then decrypts the legacy blob, accepts plaintext values and events, and re-encrypts the state into the current format. Plaintext events stay plaintext in the inner service, so a session that holds them stays readable only with migration enabled.

#### Key Management

```rust
use adk_session::EncryptionKey;

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

`schema_version()` returns 0 when the migration registry table does not exist yet.
Any other failure to read it, such as a lost connection or a missing privilege, is
returned as an error.

### Baseline Detection

If you have an existing database created before the migration system was added, `migrate()` detects the pre-existing tables and registers them as already applied. This avoids destructive re-creation and allows incremental adoption.

On PostgreSQL only tables in the connection's current schema (`current_schema()`) count, so tables that another deployment created in a different schema of the same database are not taken as a baseline.

### Migration Guarantees

- **Forward-only**: Migrations are applied in order and never rolled back
- **Idempotent**: Running `migrate()` multiple times is safe
- **Transactional**: Each SQL step runs in its own transaction and is recorded with its version, description, and time
- **Serialized**: PostgreSQL `migrate()` holds an advisory lock (`PostgresSessionService::ADVISORY_LOCK_KEY`) on one connection for the whole run, so instances starting together do not race

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

A second `create` with the same app, user, and session ID fails with `session.already_exists`
and leaves the first session as it was; see [Get or Create](#get-or-create).

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

On `InMemorySessionService`, a bare session ID that more than one app or user holds fails
with `session.ambiguous_id`; `append_event_for_identity` (see
[Typed Session Identity](#typed-session-identity)) names the session unambiguously.

### 4. Listing

List all sessions for a user:

```rust
use adk_session::ListRequest;

let sessions = service.list(ListRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    limit: None,
    offset: None,
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

1. Retrieves the session; a run for a session that does not exist fails with `session.not_found`, so create it first (see [Get or Create](#get-or-create))
2. Screens and saves the user message, then passes the session context to the agent
3. Appends events as the conversation progresses
4. Updates session state based on agent actions

```rust
use adk_rust::prelude::*;
use adk_rust::{SessionId, UserId};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let model = Arc::new(GeminiModel::new(&api_key, "gemini-3.7-flash")?);

    let agent = LlmAgentBuilder::new("assistant")
        .model(model)
        .instruction("You are a helpful assistant.")
        .build()?;

    let session_service = Arc::new(InMemorySessionService::new());
    session_service
        .create(CreateRequest {
            app_name: "my_app".to_string(),
            user_id: "user_123".to_string(),
            session_id: Some("session_abc".to_string()),
            state: HashMap::new(),
        })
        .await?;

    // Create runner with session service
    let runner = Runner::builder()
        .app_name("my_app")
        .agent(Arc::new(agent))
        .session_service(session_service)
        .build()?;

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
let session = service.get_for_identity(&identity).await?;
service.delete_for_identity(&identity).await?;
```

All session backends (in-memory, SQLite, PostgreSQL, Redis, MongoDB, Firestore, Neo4j, Vertex) support the typed identity path. The legacy `append_event(&str, ...)` method remains available for backward compatibility, except on `EncryptedSession`, which binds ciphertext to the full identity and requires `append_event_for_identity()`.

For new code, prefer `append_event_for_identity()` and the other typed identity helpers.

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
    
    /// Check if empty (default: `len() == 0`)
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
        limit: None,
        offset: None,
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
