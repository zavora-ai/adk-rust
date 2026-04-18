# adk-memory

Semantic memory and search for Rust Agent Development Kit (ADK-Rust) agents.

[![Crates.io](https://img.shields.io/crates/v/adk-memory.svg)](https://crates.io/crates/adk-memory)
[![Documentation](https://docs.rs/adk-memory/badge.svg)](https://docs.rs/adk-memory)
[![License](https://img.shields.io/crates/l/adk-memory.svg)](LICENSE)

## Overview

`adk-memory` provides long-term memory capabilities for the Rust Agent Development Kit ([ADK-Rust](https://github.com/zavora-ai/adk-rust)):

- **InMemoryMemoryService** - Simple in-memory memory storage
- **SqliteMemoryService** - SQLite-backed persistence (`sqlite-memory` feature)
- **PostgresMemoryService** - PostgreSQL + pgvector persistence (`database-memory` feature)
- **MongoMemoryService** - MongoDB-backed persistence (`mongodb-memory` feature)
- **Neo4jMemoryService** - Neo4j-backed persistence (`neo4j-memory` feature)
- **RedisMemoryService** - Redis-backed persistence (`redis-memory` feature)
- **MemoryService** - Trait for custom storage backends
- **Semantic Search** - Query memories by content similarity
- **Project-Scoped Isolation** - Isolate memories by project within a user
- **Schema Migrations** - Versioned, forward-only migrations for all database backends

## Installation

```toml
[dependencies]
adk-memory = "0.7.0"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "0.7.0", features = ["memory"] }
```

## Quick Start

```rust
use adk_memory::{InMemoryMemoryService, MemoryService, MemoryEntry, SearchRequest};
use adk_core::Content;
use chrono::Utc;

let service = InMemoryMemoryService::new();

let entries = vec![
    MemoryEntry {
        content: Content::new("user").with_text("User prefers dark mode"),
        author: "system".to_string(),
        timestamp: Utc::now(),
    },
];

service.add_session("my_app", "user_123", "session_456", entries).await?;

let response = service.search(SearchRequest {
    query: "what theme does the user like?".to_string(),
    user_id: "user_123".to_string(),
    app_name: "my_app".to_string(),
    limit: None,
    min_score: None,
    project_id: None, // None = global entries only
}).await?;

for memory in response.memories {
    println!("Found: {:?}", memory.content);
}
```

## Project-Scoped Memory

Memories can be scoped to a project within a user. The isolation key is `(app_name, user_id, project_id?)`:

- **Global entries** (`project_id = None`): visible in all project contexts and in global-only searches.
- **Project entries** (`project_id = Some(id)`): visible only when searching within that specific project.
- **Project search** (`project_id = Some(id)`): returns global entries + entries for that project.
- **Global search** (`project_id = None`): returns only global entries.

### Writing project-scoped entries

```rust
use adk_memory::{InMemoryMemoryService, MemoryService, MemoryEntry};
use adk_core::Content;
use chrono::Utc;

let service = InMemoryMemoryService::new();

// Global entry (no project scope)
service.add_session("app", "user", "sess-1", vec![entry]).await?;

// Project-scoped entry
service.add_session_to_project("app", "user", "sess-2", "my-project", vec![entry]).await?;

// Single entry to a project
service.add_entry_to_project("app", "user", "my-project", entry).await?;
```

### Searching with project scope

```rust
use adk_memory::SearchRequest;

// Global-only search — returns only global entries
let global = service.search(SearchRequest {
    query: "topic".into(),
    user_id: "user".into(),
    app_name: "app".into(),
    limit: None,
    min_score: None,
    project_id: None,
}).await?;

// Project search — returns global + project entries
let project = service.search(SearchRequest {
    query: "topic".into(),
    user_id: "user".into(),
    app_name: "app".into(),
    limit: None,
    min_score: None,
    project_id: Some("my-project".into()),
}).await?;
```

### Project-scoped deletion

```rust
// Delete entries matching a query within a project only
service.delete_entries_in_project("app", "user", "my-project", "query").await?;

// Delete ALL entries for a project
service.delete_project("app", "user", "my-project").await?;

// Global delete — only removes global entries
service.delete_entries("app", "user", "query").await?;

// GDPR delete_user — removes everything (global + all projects)
service.delete_user("app", "user").await?;
```

### MemoryServiceAdapter with project scope

```rust
use adk_memory::{InMemoryMemoryService, MemoryServiceAdapter};
use adk_core::Memory;
use std::sync::Arc;

let service = Arc::new(InMemoryMemoryService::new());

// Adapter without project — operates on global entries
let global_adapter = MemoryServiceAdapter::new(service.clone(), "app", "user");

// Adapter with project — all operations scoped to the project
let project_adapter = MemoryServiceAdapter::new(service.clone(), "app", "user")
    .with_project_id("my-project");

// Core Memory trait methods for ad-hoc project access
global_adapter.search_in_project("query", "other-project").await?;
global_adapter.add_to_project(entry, "other-project").await?;
```

### Project ID validation

```rust
use adk_memory::validate_project_id;

validate_project_id("my-project")?;  // Ok
validate_project_id("")?;            // Err: must not be empty
validate_project_id(&"x".repeat(257))?; // Err: exceeds 256 chars
```

## Feature Flags

| Feature | Backend | Description |
|---------|---------|-------------|
| `sqlite-memory` | SQLite | Single-node persistence via sqlx |
| `database-memory` | PostgreSQL | pgvector-backed semantic search |
| `redis-memory` | Redis | Low-latency in-memory persistence via fred |
| `mongodb-memory` | MongoDB | Document-oriented persistence |
| `neo4j-memory` | Neo4j | Graph database persistence |

```toml
# SQLite
adk-memory = { version = "0.7.0", features = ["sqlite-memory"] }

# PostgreSQL + pgvector
adk-memory = { version = "0.7.0", features = ["database-memory"] }
```

## Schema Migrations

All database backends (SQLite, PostgreSQL, MongoDB, Neo4j) include a versioned migration system. Migrations are forward-only, idempotent, and tracked in a registry table.

| Version | Description |
|---------|-------------|
| v1 | Initial schema (tables, indexes, FTS) |
| v2 | Add `project_id` column/index for project-scoped memory |

```rust
use adk_memory::SqliteMemoryService;

let service = SqliteMemoryService::new("sqlite:memory.db").await?;

// Run all pending migrations (v1 + v2)
service.migrate().await?;

// Check current schema version
let version = service.schema_version().await?;
println!("Schema version: {version}");
```

## MemoryService Trait

```rust
#[async_trait]
pub trait MemoryService: Send + Sync {
    // Core methods
    async fn add_session(&self, app_name: &str, user_id: &str, session_id: &str, entries: Vec<MemoryEntry>) -> Result<()>;
    async fn search(&self, req: SearchRequest) -> Result<SearchResponse>;

    // Project-scoped methods (defaults delegate to non-project versions)
    async fn add_session_to_project(&self, app_name: &str, user_id: &str, session_id: &str, project_id: &str, entries: Vec<MemoryEntry>) -> Result<()>;
    async fn add_entry_to_project(&self, app_name: &str, user_id: &str, project_id: &str, entry: MemoryEntry) -> Result<()>;
    async fn delete_entries_in_project(&self, app_name: &str, user_id: &str, project_id: &str, query: &str) -> Result<u64>;
    async fn delete_project(&self, app_name: &str, user_id: &str, project_id: &str) -> Result<u64>;

    // Lifecycle methods
    async fn delete_user(&self, app_name: &str, user_id: &str) -> Result<()>;
    async fn delete_session(&self, app_name: &str, user_id: &str, session_id: &str) -> Result<()>;
    async fn add_entry(&self, app_name: &str, user_id: &str, entry: MemoryEntry) -> Result<()>;
    async fn delete_entries(&self, app_name: &str, user_id: &str, query: &str) -> Result<u64>;
    async fn health_check(&self) -> Result<()>;
}
```

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-core](https://crates.io/crates/adk-core) - Core `Memory` trait
- [adk-runner](https://crates.io/crates/adk-runner) - Memory injection during execution
- [adk-rag](https://crates.io/crates/adk-rag) - RAG pipeline with vector stores

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
