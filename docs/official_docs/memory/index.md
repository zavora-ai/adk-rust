# Memory

Give your agents a memory that **outlives a single conversation**. Session state
holds *this* conversation; **memory** is the long-term store an agent searches
for context from earlier conversations, and that your application or the agent's
tools write durable facts into — so the next session already knows the user.

This section takes you through it from scratch: the two kinds of memory ADK-Rust
offers, the core types, the backends, the bi-temporal knowledge graph, and how to
wire it all into an agent.

> **New to ADK-Rust?** Read the [Introduction](../introduction.md) and
> [Sessions & State](../sessions/sessions.md) first — memory is the *persistent*
> counterpart to the *ephemeral* session.

## Two kinds of memory

ADK-Rust ships two complementary memory models behind the **same**
`MemoryService` trait, so an agent can use either through one interface:

| | **Semantic store** | **Knowledge graph** |
|---|---|---|
| Shape | A searchable pile of text entries | Entities, observations, and typed relations |
| Recall | Similarity / keyword search over entries | Profile card + token-scored node search |
| Best for | "What did we discuss about X?" | "What is true about this user *now*?" |
| Backends | InMemory, SQLite, Postgres, Redis, MongoDB, Neo4j, Vertex AI Memory Bank | `GraphMemoryService` (SQLite, facts with validity intervals) |
| Page | [Concepts](concepts.md) · [Backends](backends.md) | [Knowledge graph](knowledge-graph.md) |

You don't have to choose globally — pick per agent based on whether you need a
transcript-style recall or a clean, queryable model of the user.

## The mental model

```text
  Agent turn
     │  search_memory(query)         ← a tool, or PreloadMemoryTool before each model call
     ▼
  adk_core::Memory                   ← the interface the agent sees
     │  (MemoryServiceAdapter bridges to…)
     ▼
  MemoryService                      ← add_session / search / delete_user …
     ├─ InMemoryMemoryService        (dev)
     ├─ Sqlite/Postgres/Redis/…      (semantic store)
     └─ GraphMemoryService           (bi-temporal knowledge graph)
```

- A **`MemoryService`** is the storage backend: it stores entries (or graph
  nodes) and answers `search`.
- A **`MemoryServiceAdapter`** wraps it as an `adk_core::Memory`, scoped to an
  `(app_name, user_id, project_id?)`, which is what an agent's `search_memory`
  actually calls.
- The **Runner** holds the `Memory` and exposes it to the agent's tools
  (`ToolContext::search_memory`); **memory tools** let the agent search and curate
  memory deliberately. Nothing is written to memory automatically: your code calls
  `add_session` or `add_entry`, or the agent uses the graph tools.

## Install

```toml
# Semantic store — pick the backend you need
adk-memory = { version = "3.0.0", features = ["sqlite-memory"] }

# Bi-temporal knowledge graph
adk-memory = { version = "3.0.0", features = ["graph-memory"] }
```

| Feature | Adds |
|---------|------|
| *(default)* | `InMemoryMemoryService` — zero-setup, for development |
| `sqlite-memory` | `SqliteMemoryService` — file-backed semantic store |
| `graph-memory` | `GraphMemoryService` — bi-temporal knowledge graph (SQLite) |
| `database-memory` | `PostgresMemoryService` with `pgvector` |
| `redis-memory` | `RedisMemoryService` |
| `mongodb-memory` | `MongoMemoryService` |
| `neo4j-memory` | `Neo4jMemoryService` |
| `vertex-memory` | `VertexAiMemoryBankService` — Vertex AI Agent Engine Memory Bank |

## 60-second quick start

Store a couple of memories and search them back:

```rust
use adk_memory::{InMemoryMemoryService, MemoryService, MemoryEntry, SearchRequest};
use adk_core::Content;
use chrono::Utc;

# async fn run() -> anyhow::Result<()> {
let memory = InMemoryMemoryService::new();

memory.add_session("support", "alice", "s1", vec![
    MemoryEntry { content: Content::new("user").with_text("Contact preference: email, not phone."),
                  author: "user".into(), timestamp: Utc::now() },
]).await?;

let hits = memory.search(SearchRequest {
    app_name: "support".into(), user_id: "alice".into(),
    query: "contact preference".into(),
    limit: None, min_score: None, project_id: None,
}).await?;

for entry in hits.memories { println!("{:?}", entry.content); }
# Ok(()) }
```

In a real agent you rarely call `search` by hand — you give the agent
[memory tools](tools-and-agents.md) so it recalls and curates memory itself.

## Where to go next

1. **[Concepts](concepts.md)** — `MemoryEntry`, the `MemoryService` trait, search, project scoping, GDPR erasure.
2. **[Backends](backends.md)** — the stores, feature flags, and how to choose.
3. **[Knowledge graph](knowledge-graph.md)** — `GraphMemoryService` and why bi-temporal matters.
4. **[Tools & agents](tools-and-agents.md)** — wiring memory into agents; `remember`/`relate` and the memory tools.
