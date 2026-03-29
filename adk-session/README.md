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
adk-session = "0.5.0"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "0.5.0", features = ["sessions"] }
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
adk-session = { version = "0.5.0", features = ["sqlite"] }

# PostgreSQL
adk-session = { version = "0.5.0", features = ["postgres"] }

# Redis
adk-session = { version = "0.5.0", features = ["redis"] }

# Encrypted sessions
adk-session = { version = "0.5.0", features = ["encrypted-session"] }
```

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

As of v0.4.0, `DatabaseSessionService` has been renamed to `SqliteSessionService` to accurately reflect that it is a SQLite-only backend. A deprecated type alias is provided for backward compatibility:

```rust
// Old (still compiles with a deprecation warning)
use adk_session::DatabaseSessionService;

// New
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
