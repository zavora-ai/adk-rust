# adk-session

Session management and state persistence for Rust Agent Development Kit (ADK-Rust) agents.

[![Crates.io](https://img.shields.io/crates/v/adk-session.svg)](https://crates.io/crates/adk-session)
[![Documentation](https://docs.rs/adk-session/badge.svg)](https://docs.rs/adk-session)
[![License](https://img.shields.io/crates/l/adk-session.svg)](LICENSE)

## Overview

`adk-session` provides session and state management for the Rust Agent Development Kit ([ADK-Rust](https://github.com/zavora-ai/adk-rust)):

- **InMemorySessionService** - Simple in-memory session storage
- **SqliteSessionService** - SQLite-backed persistence (`sqlite` feature)
- **PostgresSessionService** - PostgreSQL-backed persistence (`postgres` feature)
- **RedisSessionService** - Redis-backed persistence (`redis` feature)
- **MongoSessionService** - MongoDB-backed persistence (`mongodb` feature)
- **Neo4jSessionService** - Neo4j-backed persistence (`neo4j` feature)
- **FirestoreSessionService** - Firestore-backed persistence (`firestore` feature)
- **VertexAiSessionService** - Vertex AI Session API backend (`vertex-session` feature)
- **Schema Migrations** - Versioned, forward-only migrations for all database backends

## Installation

```toml
[dependencies]
adk-session = "2.0.0"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "2.0.0", features = ["sessions"] }
```

## Quick Start

```rust
use adk_session::{InMemorySessionService, SessionService, CreateRequest, KEY_PREFIX_USER};
use serde_json::json;
use std::collections::HashMap;

let service = InMemorySessionService::new();

let mut initial_state = HashMap::new();
initial_state.insert(format!("{}name", KEY_PREFIX_USER), json!("Alice"));

let session = service.create(CreateRequest {
    app_name: "my_app".to_string(),
    user_id: "user_123".to_string(),
    session_id: None,
    state: initial_state,
}).await?;

let name = session.state().get("user:name");
```

## State Prefixes

| Prefix | Purpose | Persistence |
|--------|---------|-------------|
| `user:` | User preferences | Across sessions |
| `app:` | Application state | Application-wide |
| `temp:` | Temporary data | Current turn only |

## Feature Flags

| Feature | Backend | Description |
|---------|---------|-------------|
| `sqlite` | SQLite | Single-node persistence via sqlx |
| `database` | SQLite | Alias for `sqlite` (backward compat) |
| `postgres` | PostgreSQL | Production-grade relational persistence |
| `redis` | Redis | Low-latency in-memory persistence via fred |
| `mongodb` | MongoDB | Document-oriented persistence |
| `neo4j` | Neo4j | Graph database persistence |
| `firestore` | Firestore | Google Cloud Firestore persistence |
| `vertex-session` | Vertex AI | Vertex AI Session API backend |
| `encrypted-session` | AES-256-GCM | Transparent encryption at rest with key rotation |

```toml
# SQLite
adk-session = { version = "2.0.0", features = ["sqlite"] }

# PostgreSQL
adk-session = { version = "2.0.0", features = ["postgres"] }

# Redis
adk-session = { version = "2.0.0", features = ["redis"] }

# Encrypted sessions
adk-session = { version = "2.0.0", features = ["encrypted-session"] }

# Vertex AI sessions through the umbrella crate
adk-rust = { version = "2.0.0", features = ["vertex-session"] }
```

## Vertex AI Sessions

`VertexAiSessionService` stores sessions through the GA `v1` Vertex AI Agent
Engine Session API:

```rust
use adk_session::{VertexAiSessionConfig, VertexAiSessionService};

let config = VertexAiSessionConfig::new("my-project", "us-central1")
    .with_reasoning_engine("1234567890");
let service = VertexAiSessionService::new_with_adc(config)?;
```

Caller-facing session IDs remain unchanged. The backend derives a deterministic
remote ID from the complete `(app_name, user_id, session_id)` identity and stores
a protected identity marker in Vertex session state. The marker is removed from
all returned state and cannot be supplied through create state or event state
deltas. This permits the same logical session ID to exist safely across apps and
users, including when several apps share one reasoning engine.

The default endpoint depends on the configured location:

| Location | Endpoint |
|----------|----------|
| `global` | `https://aiplatform.googleapis.com` |
| `us` | `https://aiplatform.us.rep.googleapis.com` |
| `eu` | `https://aiplatform.eu.rep.googleapis.com` |
| Region such as `us-central1` | `https://us-central1-aiplatform.googleapis.com` |

### Production boundaries

| Boundary | Behavior |
|----------|----------|
| Vertex user ID | At most 128 Unicode scalar values |
| Encoded request body | 64 MiB per create/append request |
| Decoded response body | 64 MiB per response and in aggregate for one paginated list operation |
| Pagination | 120-second total deadline; recent-event bounds and timestamp filters are pushed into the API |
| Page token | At most 64 KiB |
| Transport | 10-second connect, 30-second credential-header, and 120-second HTTP request deadlines |
| Long-running operations | 120-second local polling deadline |
| JSON and Struct values | At most 64 nested levels |

Use `with_max_request_bytes()`, `with_max_response_bytes()`, and
`with_pagination_timeout()` to select deployment-specific bounds. Raising a
body limit weakens the default allocation protection; neither body-byte budget
is a total process-memory ceiling.

Custom endpoints are security-sensitive: the origin receives Google
authorization headers and complete session/event payloads. The caller must
configure a trusted endpoint. The constructor accepts an HTTPS origin shape
without userinfo, a path, query, or fragment; loopback HTTP is available for
tests. Redirects are disabled.

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
ambiguous and non-retryable. The backend does not throttle or retry internally.
Size capacity using the current [Vertex AI
quotas](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/quotas).

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

Arbitrary Google `rawEvent` Struct values remain opaque. Reappend preserves
every original key and value and adds the reserved `_adkRust` envelope; removing
that envelope yields the original Struct. A pre-existing malformed `_adkRust`
key fails closed. Google ADK projection is best-effort even when a Struct has a
non-empty string `id`, numeric `timestamp`, and string `invocationId` and
`author`; incompatible content, actions, metadata, or optional fields do not
reject the canonical SessionEvent.

The GA `v1` `FunctionCall` and `FunctionResponse` wire messages have no `id`.
ID-bearing ADK function parts and empty or noncanonical Base64 thought-signature
bytes use the lossless private `rawEvent` path. `Part.mediaResolution` accepts a
bounded JSON object on any otherwise-valid canonical part. Top-level
`inlineData`/`fileData` `displayName` values are accepted from the deployed GA
wire. ADK projection omits these provider-only fields, while the canonical
sidecar preserves them across reappend. Private-envelope validation treats
omitted proto3 default scalar fields as equivalent to their empty private
values, accepts safe integer/float normalization inside Vertex Struct values,
and restores exact private scalar presence.

### Legacy unmarked sessions

When `reasoning_engine` is omitted, `app_name` must be the canonical nonzero
numeric reasoning-engine ID without leading zeros. That ID selects the isolated
parent, and the backend can access unmarked Python ADK or pre-v2 sessions by
their direct remote ID. Logical app names and full resource names require
`with_reasoning_engine()`. When a fixed reasoning engine is shared, unmarked
access is disabled by default. Enable it for exactly one audited app during
migration:

```rust
use adk_session::{VertexAiSessionConfig, VertexAiSessionService};

let legacy_config = VertexAiSessionConfig::new("my-project", "us-central1")
    .with_reasoning_engine("1234567890");
let service = VertexAiSessionService::new_with_adc(legacy_config)?
    .allow_unmarked_sessions_for_app("legacy-app");
```

> **Important:** Unmarked sessions carry no app marker. Only enable this
> compatibility mode when the selected app exclusively owns the legacy
> sessions in that reasoning engine. The backend still requires the exact
> `user_id` and rejects marked-direct, reserved-ID, and computed/direct
> ambiguities.

Use `append_event_for_identity()` for new code. The legacy
`append_event(session_id, event)` method works only after a create, get, or list
operation has cached exactly one app/user scope for that ID. The cache is
bounded; long-running processes may need to get or list an older session again
after its scope is evicted.

## Encrypted Sessions

Wrap any `SessionService` with `EncryptedSession` to encrypt session state at rest using AES-256-GCM:

```rust
use adk_session::{EncryptedSession, EncryptionKey, InMemorySessionService};

let key = EncryptionKey::generate();
let inner = InMemorySessionService::new();
let service = EncryptedSession::new(inner, key, vec![]);

// Use like any SessionService — encryption is transparent
```

Key rotation is supported by passing previous keys:

```rust
let new_key = EncryptionKey::generate();
let old_key = EncryptionKey::from_env("OLD_KEY")?;
let service = EncryptedSession::new(inner, new_key, vec![old_key]);
```

## Schema Migrations

All database backends (SQLite, PostgreSQL, MongoDB, Neo4j) include a versioned migration system. Migrations are forward-only, idempotent, and tracked in a `_schema_migrations` registry table.

```rust
use adk_session::SqliteSessionService;

let service = SqliteSessionService::new("sqlite:sessions.db").await?;

// Run all pending migrations
service.migrate().await?;

// Check current schema version
let version = service.schema_version().await?;
println!("Schema version: {version}");
```

Each backend detects pre-existing tables (baseline detection) and registers them as already applied, so `migrate()` is safe to call on both fresh and existing databases.

## Rename: DatabaseSessionService → SqliteSessionService

As of v0.4.0, `DatabaseSessionService` was renamed to `SqliteSessionService` to accurately reflect that it is a SQLite-only backend. The deprecated type alias was removed in v0.7.0. Update your imports:

```rust
use adk_session::SqliteSessionService;
```

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-core](https://crates.io/crates/adk-core) - Core `Session` trait
- [adk-runner](https://crates.io/crates/adk-runner) - Uses sessions for execution

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
