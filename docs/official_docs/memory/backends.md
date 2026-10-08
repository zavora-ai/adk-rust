# Memory Backends

All backends implement the same [`MemoryService`](concepts.md#the-memoryservice-trait)
trait, so you choose one by enabling a feature flag and constructing it — your
agent code doesn't change. There are six.

## At a glance

| Backend | Feature | Persistence | Search | Use when |
|---------|---------|-------------|--------|----------|
| `InMemoryMemoryService` | *(default)* | Process only | Keyword | Development, tests, demos |
| `SqliteMemoryService` | `sqlite-memory` | A file | Keyword | Single-node apps, local persistence |
| `PostgresMemoryService` | `database-memory` | Postgres + `pgvector` | **Vector similarity** | Production semantic search |
| `RedisMemoryService` | `redis-memory` | Redis (optional TTL) | Keyword | Fast, ephemeral-ish, shared cache |
| `MongoMemoryService` | `mongodb-memory` | MongoDB | Keyword / vector | Existing Mongo infra |
| `Neo4jMemoryService` | `neo4j-memory` | Neo4j | Graph | Existing Neo4j infra |

(The bi-temporal **`GraphMemoryService`** is a seventh, covered on its own page —
[Knowledge graph](knowledge-graph.md).)

## InMemory — start here

Zero setup. Everything lives in the process and disappears on exit.

```rust
use adk_memory::InMemoryMemoryService;
let memory = InMemoryMemoryService::new();
```

Perfect for development and the test suite; swap it for a durable backend with a
one-line change because the trait is identical.

## SQLite — a file

Durable, single-file, no server. Great for desktop apps and single-node services.

```rust
use adk_memory::SqliteMemoryService;
let memory = SqliteMemoryService::new("sqlite://memory.db").await?;
// or build from an existing pool: SqliteMemoryService::from_pool(pool)
```

Search treats the query as plain text: an entry matches when it contains every
word of the query, and FTS5 syntax such as `'`, `?`, `*`, `AND` or `OR` is
matched literally, the same as `plainto_tsquery` in the Postgres backend.

## Postgres + pgvector — production semantic search

The backend for real similarity search. It stores embeddings in `pgvector` and
lets you pick the index strategy.

```rust
use adk_memory::{PostgresMemoryService, VectorIndexType};

let memory = PostgresMemoryService::builder(pool, embedding_provider)
    .vector_index(VectorIndexType::Hnsw { m: 32, ef_construction: 128 })  // or IvfFlat / None
    .build()
    .await?;
```

`VectorIndexType::None` does exact (brute-force) search — fine for small sets;
`Hnsw` (the default) and `IvfFlat` scale to large ones. Vector search needs an
[embedding provider](#embeddings).

## Redis, MongoDB, Neo4j

Use these when you already run the infrastructure:

```rust
use adk_memory::{RedisMemoryService, RedisMemoryConfig};
use std::time::Duration;

let memory = RedisMemoryService::new(RedisMemoryConfig {
    url: "redis://localhost:6379".into(),
    ttl: Some(Duration::from_secs(60 * 60 * 24 * 30)),   // optional expiry
}).await?;
```

`MongoMemoryService::new(...)` and `Neo4jMemoryService::new(...)` follow the same
shape. All of them honor the `(app, user, project)` isolation from
[Concepts](concepts.md#isolation-app-user-and-project).

| Backend | Isolation detail |
|---------|------------------|
| Redis | Key segments are percent-encoded (`%` → `%25`, `:` → `%3A`), so app `a:b` with user `c` never shares keys with app `a` and user `b:c`. `delete_user` matches only the named user's keys, including ids such as `*`. |
| Neo4j | Entry ids are scoped by app, user, project and session, so ingesting the same session again updates it. Vector search over-fetches `limit × 10` candidates (capped at 1000) before filtering to the tenant; raise the cap with `with_vector_candidate_cap` when one index holds many tenants. |

## Embeddings

Backends that do **vector** similarity (Postgres, optionally Mongo) need to turn
text into vectors. Provide an `EmbeddingProvider`:

```rust
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}
```

Keyword backends (InMemory, SQLite, Redis) don't need one. Match the embedding
model you index with the one you query with.

## Migrations

The SQL-backed services run **versioned, idempotent migrations** to create and
upgrade their schema, so deploying a new version doesn't require manual DDL. Call
the service's `migrate()` (or let construction handle it) on startup.

## Choosing

- **Building / testing** → InMemory.
- **One node, want it to persist** → SQLite.
- **Real semantic recall at scale** → Postgres + pgvector.
- **You already run Redis / Mongo / Neo4j** → that backend.
- **You want a queryable model of the user, not a transcript** →
  [`GraphMemoryService`](knowledge-graph.md).

Next: [Knowledge graph →](knowledge-graph.md)
