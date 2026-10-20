# Memory Concepts

The semantic-store side of memory is built from a handful of small types. Learn
these four and the rest of the section follows.

## `MemoryEntry`

A single memory record: some content, who authored it, and when.

```rust
use adk_memory::MemoryEntry;
use adk_core::Content;
use chrono::Utc;

let entry = MemoryEntry {
    content: Content::new("user").with_text("I prefer dark mode"),
    author: "user".to_string(),
    timestamp: Utc::now(),
};
```

Entries are the unit of storage and the unit of recall — `search` returns the
entries most relevant to a query.

## The `MemoryService` trait

Every backend implements one trait. Two methods are required; the rest have
default implementations a backend can override:

```rust
#[async_trait]
pub trait MemoryService: Send + Sync {
    // Required
    async fn add_session(&self, app: &str, user: &str, session: &str,
                         entries: Vec<MemoryEntry>) -> Result<()>;
    async fn search(&self, req: SearchRequest) -> Result<SearchResponse>;

    // Optional (default: an error naming the method)
    async fn delete_user(&self, app: &str, user: &str) -> Result<()>;          // GDPR
    async fn delete_session(&self, app: &str, user: &str, session: &str) -> Result<()>;
    async fn add_entry(&self, app: &str, user: &str, entry: MemoryEntry) -> Result<()>;
    async fn delete_entries(&self, app: &str, user: &str, query: &str) -> Result<u64>;
    async fn list_recent(&self, app: &str, user: &str, limit: usize) -> Result<Vec<MemoryEntry>>;
    // …plus health_check (default: Ok), supports_project_scoping (default: false),
    // and the project-scoped methods below
}
```

- **`add_session`** ingests a conversation's worth of entries. The `Runner` never calls
  it: call it from your application, for example when a conversation ends.
  `IntegratedRealtimeRunner` calls it for each completed turn unless
  `IntegrationConfig::store_to_memory` is off.
- **`search`** is recall. Backends interpret the query differently — keyword,
  embedding similarity, or graph token-scoring — but the contract is the same.
- **`list_recent`** lists a user's newest entries, global and project-scoped, with no
  query — for "what does the agent remember?" views.

## `SearchRequest` / `SearchResponse`

```rust
use adk_memory::{MemoryService, SearchRequest};

let req = SearchRequest {
    app_name: "support".into(),
    user_id:  "alice".into(),
    query:    "contact preference".into(),
    limit: Some(5),       // None = 10
    min_score: None,      // drop matches scoring below this (0.0–1.0)
    project_id: None,     // Some(id) = global + that project's entries
};

let resp = memory.search(req).await?;   // resp.memories: Vec<MemoryEntry>
```

## Session state vs. memory

These are different tools — don't conflate them:

| | **Session state** | **Memory** |
|---|---|---|
| Lifetime | One conversation | Across conversations |
| API | `adk-session` (`SessionService`, state map) | `adk-memory` (`MemoryService`) |
| Holds | The live transcript + scratch state | Durable facts worth recalling later |
| Read | Always in context | On demand, via `search_memory` |

A typical flow: the conversation lives in **session state**; when it ends (or each
turn), salient parts are written to **memory**; the next session **searches**
memory to rehydrate context. See [Sessions & State](../sessions/sessions.md).

## Isolation: app, user, and project

Memory is always keyed by `(app_name, user_id)`, so users never see each other's
memories. You can scope a third level — **project** — within a user:

- **Global entries** (`project_id = None`) — visible in every context.
- **Project entries** (`project_id = Some(id)`) — visible only within that project.
- **Project search** returns global **+** matching project entries; **global
  search** returns only global entries.

```rust
use adk_memory::{InMemoryMemoryService, MemoryService, MemoryServiceAdapter};
use std::sync::Arc;

let service = Arc::new(InMemoryMemoryService::new());

// store within a project
service.add_session_to_project("app", "user", "sess", "acme-project", entries).await?;

// an adapter scoped to that project
let adapter = MemoryServiceAdapter::new(service, "app", "user")
    .with_project_id("acme-project");
```

Use projects to keep, say, a user's *work* and *personal* assistants from sharing
memory while still sharing truly global facts.

### Not every backend implements project scoping

Project isolation is a backend capability, not something the trait can guarantee.
Ask before relying on it:

```rust
use adk_memory::MemoryService;

if service.supports_project_scoping() {
    service.add_entry_to_project("app", "user", "acme-project", entry).await?;
} else {
    // Decide explicitly: refuse, or store globally on purpose.
}
```

| Backend | Project scoping |
|---------|-----------------|
| `InMemoryMemoryService` | Yes |
| `SqliteMemoryService` | Yes |
| `PostgresMemoryService` | Yes |
| `RedisMemoryService` | Yes |
| `MongoMemoryService` | Yes |
| `Neo4jMemoryService` | Yes |
| `GraphMemoryService` | **No** — project methods return an error |
| `VertexAiMemoryBankService` | **No** — project methods return an error |

A backend without project support **fails** the call rather than silently writing or
deleting globally. Widening the scope of a write would make data intended for one
project visible to everything under the same app and user, and widening a delete
would remove entries outside the named project — neither is something a caller could
detect from the return value. `MemoryServiceAdapter::supports_project_scoping`
reports what its backend supports.

## GDPR erasure

`delete_user(app, user)` removes **all** of a user's memories (entries and
embeddings) across all projects — the right-to-erasure primitive. Backends that
store durably implement it; call it from your account-deletion path.

```rust
use adk_memory::MemoryService;

memory.delete_user("support", "alice").await?;
```

Next: [Backends →](backends.md)
