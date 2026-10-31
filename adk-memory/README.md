# adk-memory

Semantic memory and search for Rust Agent Development Kit (ADK-Rust) agents.

[![Crates.io](https://img.shields.io/crates/v/adk-memory.svg)](https://crates.io/crates/adk-memory)
[![Documentation](https://docs.rs/adk-memory/badge.svg)](https://docs.rs/adk-memory)
[![License](https://img.shields.io/crates/l/adk-memory.svg)](../LICENSE)

## Overview

`adk-memory` provides long-term memory capabilities for the Rust Agent Development Kit ([ADK-Rust](https://github.com/zavora-ai/adk-rust)):

- **InMemoryMemoryService** - Simple in-memory memory storage
- **SqliteMemoryService** - SQLite-backed persistence (`sqlite-memory` feature)
- **PostgresMemoryService** - PostgreSQL + pgvector persistence (`database-memory` feature)
- **MongoMemoryService** - MongoDB-backed persistence (`mongodb-memory` feature)
- **Neo4jMemoryService** - Neo4j-backed persistence (`neo4j-memory` feature)
- **RedisMemoryService** - Redis-backed persistence (`redis-memory` feature)
- **VertexAiMemoryBankService** - Vertex AI Agent Engine Memory Bank (`vertex-memory` feature)
- **GraphMemoryService** - Bi-temporal knowledge graph of entities, relations, and observations on SQLite (`graph-memory` feature)
- **MemoryService** - Trait for custom storage backends
- **Search** - Keyword search on the in-memory, SQLite, and Redis backends; vector similarity with an `EmbeddingProvider` on PostgreSQL (pgvector, with a `tsvector` keyword fallback), MongoDB (Atlas Vector Search, with a text-search fallback), and Neo4j
- **Project-Scoped Isolation** - Isolate memories by project within a user
- **Schema Migrations** - Versioned, forward-only migrations for the SQL, MongoDB, and Neo4j backends

Agents reach memory through the runner, which attaches a memory service to each invocation (`ToolContext::search_memory`), and through `adk-tool`'s `memory-tools` feature: `LoadMemoryTool`, and `PreloadMemoryTool`, which also converts into a `BeforeModelCallback` that injects memories at the start of a turn.

## Installation

```toml
[dependencies]
adk-memory = "3.0.0"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "3.0.0", features = ["memory"] }
```

## Quick Start

```rust
use adk_core::Content;
use adk_memory::{InMemoryMemoryService, MemoryEntry, MemoryService, SearchRequest};
use chrono::Utc;

#[tokio::main]
async fn main() -> adk_core::Result<()> {
    let service = InMemoryMemoryService::new();

    let entries = vec![MemoryEntry {
        content: Content::new("user").with_text("The user prefers the dark theme."),
        author: "system".to_string(),
        timestamp: Utc::now(),
    }];

    service.add_session("my_app", "user_123", "session_456", entries).await?;

    let response = service
        .search(SearchRequest {
            query: "dark theme".to_string(),
            user_id: "user_123".to_string(),
            app_name: "my_app".to_string(),
            limit: None,
            min_score: None,
            project_id: None, // None = global entries only
        })
        .await?;

    assert_eq!(response.memories.len(), 1);
    for memory in response.memories {
        println!("Found: {:?}", memory.content);
    }
    Ok(())
}
```

The in-memory and Redis backends split text into runs of Unicode letters and digits, score each entry by the share of query words it contains (`min_score` filters on that score), and return the highest-scoring, then newest, entries.

## Project-Scoped Memory

Memories can be scoped to a project within a user. The isolation key is `(app_name, user_id, project_id?)`:

- **Global entries** (`project_id = None`): visible in all project contexts and in global-only searches.
- **Project entries** (`project_id = Some(id)`): visible only when searching within that specific project.
- **Project search** (`project_id = Some(id)`): returns global entries + entries for that project.
- **Global search** (`project_id = None`): returns only global entries.

### Writing project-scoped entries

```rust
use adk_core::Content;
use adk_memory::{InMemoryMemoryService, MemoryEntry, MemoryService};
use chrono::Utc;

#[tokio::main]
async fn main() -> adk_core::Result<()> {
    let service = InMemoryMemoryService::new();
    let entry = MemoryEntry {
        content: Content::new("user").with_text("Launch is planned for March"),
        author: "user".to_string(),
        timestamp: Utc::now(),
    };

    // Global entry (no project scope)
    service.add_session("app", "user", "sess-1", vec![entry.clone()]).await?;

    // Project-scoped entries
    service
        .add_session_to_project("app", "user", "sess-2", "my-project", vec![entry.clone()])
        .await?;

    // Single entry to a project
    service.add_entry_to_project("app", "user", "my-project", entry).await?;
    Ok(())
}
```

### Searching with project scope

```rust
use adk_memory::{MemoryService, SearchRequest, SearchResponse};

async fn search(service: &dyn MemoryService) -> adk_core::Result<(SearchResponse, SearchResponse)> {
    // Global-only search — returns only global entries
    let global = service
        .search(SearchRequest {
            query: "topic".into(),
            user_id: "user".into(),
            app_name: "app".into(),
            limit: None,
            min_score: None,
            project_id: None,
        })
        .await?;

    // Project search — returns global + project entries
    let project = service
        .search(SearchRequest {
            query: "topic".into(),
            user_id: "user".into(),
            app_name: "app".into(),
            limit: None,
            min_score: None,
            project_id: Some("my-project".into()),
        })
        .await?;
    Ok((global, project))
}
```

### Project-scoped deletion

```rust
use adk_memory::MemoryService;

async fn delete(service: &dyn MemoryService) -> adk_core::Result<()> {
    // Delete entries matching a query within a project only
    service.delete_entries_in_project("app", "user", "my-project", "query").await?;

    // Delete ALL entries for a project
    service.delete_project("app", "user", "my-project").await?;

    // Global delete — only removes global entries
    service.delete_entries("app", "user", "query").await?;

    // delete_user removes everything (global + all projects)
    service.delete_user("app", "user").await?;
    Ok(())
}
```

### MemoryServiceAdapter with project scope

```rust
use std::sync::Arc;

use adk_core::{Content, Memory, MemoryEntry};
use adk_memory::{InMemoryMemoryService, MemoryServiceAdapter};

#[tokio::main]
async fn main() -> adk_core::Result<()> {
    let service = Arc::new(InMemoryMemoryService::new());

    // Adapter without project — operates on global entries
    let global_adapter = MemoryServiceAdapter::new(service.clone(), "app", "user");

    // Adapter with project — all operations scoped to the project
    let _project_adapter =
        MemoryServiceAdapter::new(service.clone(), "app", "user").with_project_id("my-project");

    // Core Memory trait methods for ad-hoc project access
    let entry = MemoryEntry {
        content: Content::new("user").with_text("Budget approved"),
        author: "user".to_string(),
    };
    global_adapter.add_to_project(entry, "other-project").await?;
    let found = global_adapter.search_in_project("budget", "other-project").await?;
    assert_eq!(found.len(), 1);
    Ok(())
}
```

### Project ID validation

```rust
use adk_memory::validate_project_id;

fn main() {
    assert!(validate_project_id("my-project").is_ok());
    assert!(validate_project_id("").is_err()); // must not be empty
    assert!(validate_project_id(&"x".repeat(257)).is_err()); // longer than 256 bytes
}
```

## Feature Flags

| Feature | Backend | Description |
|---------|---------|-------------|
| `sqlite-memory` | SQLite | Single-node persistence via sqlx |
| `database-memory` | PostgreSQL | pgvector-backed semantic search |
| `redis-memory` | Redis | Low-latency in-memory persistence via fred |
| `mongodb-memory` | MongoDB | Document-oriented persistence |
| `neo4j-memory` | Neo4j | Graph database persistence |
| `vertex-memory` | Vertex AI Memory Bank | Managed memory generation and retrieval on the Gemini Enterprise Agent Platform |
| `graph-memory` | SQLite | `GraphMemoryService` knowledge graph (implies `sqlite-memory`) |
| `embedding-trait` | — | The `EmbeddingProvider` trait without a vector backend |

```toml
# SQLite
adk-memory = { version = "3.0.0", features = ["sqlite-memory"] }

# PostgreSQL + pgvector
adk-memory = { version = "3.0.0", features = ["database-memory"] }
```

## Backend Notes

| Backend | Behaviour |
|---------|-----------|
| SQLite | Search matches entries containing every query word; FTS5 syntax in the query is matched literally. |
| Redis | Key segments are percent-encoded (`%` → `%25`, `:` → `%3A`); identifiers without either character keep their keys. `delete_user` deletes only the named user's keys. |
| Neo4j | Entry ids are scoped by app, user, project and session; `add_session` merges on them, so re-ingesting a session updates it. Vector search over-fetches candidates before filtering to the tenant — see `Neo4jMemoryService::with_vector_candidate_cap`. |

## Schema Migrations

The SQLite, PostgreSQL, MongoDB, and Neo4j backends include a versioned migration system. Migrations are forward-only, idempotent, and tracked in a registry table. `schema_version()` returns 0 only when the registry table does not exist; a failed read is an error. PostgreSQL `migrate()` holds the advisory lock `PostgresMemoryService::ADVISORY_LOCK_KEY` on one connection.

| Version | Description |
|---------|-------------|
| v1 | Initial schema (tables, indexes, FTS) |
| v2 | Add `project_id` column/index for project-scoped memory |

```rust
use adk_memory::SqliteMemoryService;

#[tokio::main]
async fn main() -> adk_core::Result<()> {
    let service = SqliteMemoryService::new("sqlite:memory.db?mode=rwc").await?;

    // Run all pending migrations
    service.migrate().await?;

    // Check the current schema version
    let version = service.schema_version().await?;
    println!("Schema version: {version}");
    Ok(())
}
```

## MemoryService Trait

| Method | Default |
|--------|---------|
| `add_session(app, user, session, entries)` | required |
| `search(SearchRequest)` | required |
| `delete_user`, `delete_session`, `add_entry`, `delete_entries`, `list_recent` | Return a "not implemented" error; backends override them |
| `export_user(app, user)` | Returns `subject.unsupported`; every backend in this crate lists the user's entries, global and project-scoped |
| `health_check()` | `Ok(())` |
| `supports_project_scoping()` | `false` |
| `add_session_to_project`, `add_entry_to_project`, `delete_entries_in_project`, `delete_project` | Return an error |

The project methods refuse instead of falling back to global writes or deletes, which would cross the project boundary. Every backend in this crate except `VertexAiMemoryBankService` and `GraphMemoryService` implements them and returns `true` from `supports_project_scoping()`.

## Subject Export and Erasure

`MemorySubjectData` adapts any `MemoryService` to `adk_core::subject::SubjectDataStore`: it
exports a subject's entries through `export_user` and erases them through `delete_user`, for
each of the subject's user ids in each covered application. Every backend passes
`adk_core::subject_data_store_conformance_tests!`.

```rust
use std::sync::Arc;

use adk_core::subject::{InMemoryConsentStore, InMemorySubjectRegistry, SubjectRights};
use adk_memory::{InMemoryMemoryService, MemorySubjectData};

let rights = SubjectRights::new(
    Arc::new(InMemorySubjectRegistry::new()),
    Arc::new(InMemoryConsentStore::new()),
)
.with_app("support")
.with_store(Arc::new(MemorySubjectData::new(Arc::new(InMemoryMemoryService::new()))));
```

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-core](https://crates.io/crates/adk-core) - Core `Memory` trait
- [adk-runner](https://crates.io/crates/adk-runner) - Attaches the memory service to each invocation
- [adk-tool](https://crates.io/crates/adk-tool) - `LoadMemoryTool` and `PreloadMemoryTool` (`memory-tools` feature)
- [adk-rag](https://crates.io/crates/adk-rag) - RAG pipeline with vector stores

## Changes in 3.0

| Change | Migration |
|--------|-----------|
| SQL backends use SQLx 0.9; `sqlx` and `pgvector` are no longer implicit features, and pgvector moves to 0.4.2 | Depend on SQLx 0.9 when sharing a pool; enable `sqlite-memory` or `database-memory` |
| `schema_version()` returns read failures as errors | — |
| Keyword search matches through punctuation and returns the best matches first | — |
| PostgreSQL `migrate()` releases its advisory lock | — |

See the [2.x to 3.0 migration guide](../docs/official_docs/migration/2.x-to-3.0.md) and the [changelog](../CHANGELOG.md) for the full list.

## SQLx compatibility

ADK-Rust 3.0 SQL backends use SQLx 0.9. SQLx types appear in this crate's public
API — `from_pool` on `SqliteMemoryService`, `GraphMemoryService` and `PostgresMemoryService`, and the `migration` runners — so an application that passes in its own pool must depend on
SQLx 0.9 too. Applications that let ADK open its own connections need no SQLx
dependency. The [SQLx 0.9 migration guide](../docs/official_docs/migration/sqlx-0.9.md)
lists who is affected and what changed, and its
[TLS behaviour](../docs/official_docs/migration/sqlx-0.9.md#tls-behaviour) section
covers PostgreSQL certificate verification.

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
