# Memory Backends

All backends implement the same [`MemoryService`](concepts.md#the-memoryservice-trait)
trait, so you choose one by enabling a feature flag and constructing it — your
agent code doesn't change.

## At a glance

| Backend | Feature | Persistence | Search | Use when |
|---------|---------|-------------|--------|----------|
| `InMemoryMemoryService` | *(default)* | Process only | Keyword | Development, tests, demos |
| `SqliteMemoryService` | `sqlite-memory` | A file | Keyword | Single-node apps, local persistence |
| `PostgresMemoryService` | `database-memory` | Postgres + `pgvector` | **Vector similarity** with an embedding provider; full-text (`tsvector`) without | Production semantic search |
| `RedisMemoryService` | `redis-memory` | Redis (optional TTL) | Keyword | Fast, ephemeral-ish, shared cache |
| `MongoMemoryService` | `mongodb-memory` | MongoDB | Atlas Vector Search with an embedding provider; text search without | Existing Mongo infra |
| `Neo4jMemoryService` | `neo4j-memory` | Neo4j | Vector index with an embedding provider; entries linked by `FOLLOWS` | Existing Neo4j infra |
| `VertexAiMemoryBankService` | `vertex-memory` | Vertex AI Agent Engine Memory Bank | Memory Bank similarity retrieval | Gemini Enterprise Agent Platform deployments |

(**`GraphMemoryService`** is covered on its own page — [Knowledge graph](knowledge-graph.md).)

## InMemory — start here

Zero setup. Everything lives in the process and disappears on exit.

```rust
use adk_memory::InMemoryMemoryService;
let memory = InMemoryMemoryService::new();
```

Perfect for development and the test suite; swap it for a durable backend with a
one-line change because the trait is identical.

Search is keyword-based, and `RedisMemoryService` searches the same way:

| Step | Behaviour |
|------|-----------|
| **Words** | A word is a run of Unicode letters and digits, lowercased, so punctuation never sticks to it: `Hometown: Nairobi.` matches `hometown` and `nairobi`. CJK text also yields character unigrams and bigrams. |
| **Match** | An entry matches when it shares at least one word with the query. |
| **Score** | The share of the query's words the entry contains, from 0.0 to 1.0. `SearchRequest::min_score` filters on it. |
| **Order** | Highest score first, then newest; `limit` (default 10) keeps the best. |
| **Entries without words** | Entries with no text, such as an image, are stored and returned by `list_recent`, and never match a query. |

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

// embedding_provider: Arc<dyn EmbeddingProvider>
let memory = PostgresMemoryService::builder("postgres://localhost/app", Some(embedding_provider))
    .vector_index(VectorIndexType::Hnsw { m: 32, ef_construction: 128 })  // or IvfFlat / None
    .build()
    .await?;
memory.migrate().await?;
```

`VectorIndexType::None` does exact (brute-force) search — fine for small sets;
`Hnsw` (the default, `m: 16, ef_construction: 64`) and `IvfFlat` scale to large ones.
Vector search needs an [embedding provider](#embeddings); with `None` the service ranks
entries by PostgreSQL full-text search (`plainto_tsquery`). Above 2000 embedding dimensions
the index uses `halfvec`, since pgvector indexes `vector` only up to 2000.

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

MongoDB and Neo4j take a connected client (from the `mongodb` and `neo4rs` crates, added to
your own dependencies) and an optional embedding provider:

```rust
use adk_memory::{MongoMemoryService, Neo4jMemoryService};

let client = mongodb::Client::with_uri_str("mongodb://localhost:27017").await?;
let mongo = MongoMemoryService::new(client, "adk_memory", None)?;
mongo.migrate().await?;

let graph = neo4rs::Graph::new("bolt://localhost:7687", "neo4j", "password").await?;
let neo4j = Neo4jMemoryService::new(graph, None)?;
neo4j.migrate().await?;
```

All of them honor the `(app, user, project)` isolation from
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
    fn dimensions(&self) -> usize;
}
```

Keyword backends (InMemory, SQLite, Redis) don't need one. Match the embedding
model you index with the one you query with.

## Migrations

The SQLite, PostgreSQL, MongoDB, Neo4j, and graph services run **versioned, idempotent
migrations** to create and upgrade their schema, so deploying a new version doesn't require
manual DDL. Construction does not migrate: call the service's `migrate()` on startup.

## Choosing

- **Building / testing** → InMemory.
- **One node, want it to persist** → SQLite.
- **Real semantic recall at scale** → Postgres + pgvector.
- **You already run Redis / Mongo / Neo4j** → that backend.
- **You want a queryable model of the user, not a transcript** →
  [`GraphMemoryService`](knowledge-graph.md).

Next: [Knowledge graph →](knowledge-graph.md)
