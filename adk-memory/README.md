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
- **Schema Migrations** - Versioned, forward-only migrations for all database backends

## Installation

```toml
[dependencies]
adk-memory = "0.3.2"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "0.3.2", features = ["memory"] }
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
}).await?;

for memory in response.memories {
    println!("Found: {:?}", memory.content);
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

```toml
# SQLite
adk-memory = { version = "0.3.2", features = ["sqlite-memory"] }

# PostgreSQL + pgvector
adk-memory = { version = "0.3.2", features = ["database-memory"] }
```

## Schema Migrations

All database backends (SQLite, PostgreSQL, MongoDB, Neo4j) include a versioned migration system. Migrations are forward-only, idempotent, and tracked in a `_schema_migrations` registry table.

```rust
use adk_memory::SqliteMemoryService;

let service = SqliteMemoryService::new("sqlite:memory.db").await?;

// Run all pending migrations
service.migrate().await?;

// Check current schema version
let version = service.schema_version().await?;
println!("Schema version: {version}");
```

## MemoryService Trait

```rust
#[async_trait]
pub trait MemoryService: Send + Sync {
    async fn add_session(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        entries: Vec<MemoryEntry>,
    ) -> Result<()>;
    
    async fn search(&self, req: SearchRequest) -> Result<SearchResponse>;
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
