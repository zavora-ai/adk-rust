# Memory Concepts

The semantic-store side of memory is built from a handful of small types. Learn
these four and the rest of the section follows.

## `MemoryEntry`

A single memory record: some content, who authored it, when, its id, and when it expires.

```rust
use adk_core::Content;
use adk_memory::MemoryEntry;
use std::time::Duration;

let entry = MemoryEntry::new(Content::new("user").with_text("I prefer dark mode"), "user");
let parking = MemoryEntry::new(Content::new("user").with_text("Parked on level 3"), "user")
    .with_id("parking-today")
    .with_ttl(Duration::from_secs(8 * 60 * 60));
```

Entries are the unit of storage and the unit of recall — `search` returns the
entries most relevant to a query.

| Field | Set with | Meaning |
|-------|----------|---------|
| `id` | `with_id`, or the backend when it stores the entry | The handle `delete_memory` takes; every entry a backend returns carries one |
| `expires_at` | `with_ttl`, `with_expires_at`, or the service's default TTL | When the entry stops being recalled; `None` keeps it until it is deleted |

`MemoryEntry` is `#[non_exhaustive]`: build it with `MemoryEntry::new` and the `with_*` methods.

### Artifact references

A memory can refer to an artifact — the frame a caption describes, the page a note cites —
without holding its bytes. `with_artifact(uri, mime_type)` adds a `Part::FileData` after the
entry's text, and `artifact_references()` returns the entry's references.

```rust
use adk_core::Content;
use adk_memory::MemoryEntry;
use adk_memory::text::{ArtifactReference, extract_text};

let caption = MemoryEntry::new(Content::new("model").with_text("carton 00117, seal torn"), "inspector")
    .with_artifact("artifact://inspector/plant-7/week-2026-11-17/vision-ab12.png#0", "image/png");
assert_eq!(extract_text(&caption.content), "carton 00117, seal torn");
assert_eq!(
    caption.artifact_references(),
    vec![ArtifactReference::new("artifact://inspector/plant-7/week-2026-11-17/vision-ab12.png#0", "image/png")]
);
```

The text stays what a search matches: `text::extract_text` returns the text parts alone, and
`text::artifact_references` the references — file data, and inline data or embedded resources
that name their source URI, never their bytes.

| Backend | Artifact references |
|---------|---------------------|
| In-memory, SQLite, PostgreSQL, MongoDB, Neo4j, Redis | Kept with the entry's whole content |
| Knowledge graph (episodic log) | Kept in a column of their own and returned as `Part::FileData` |
| Vertex AI Memory Bank | Not kept: Memory Bank stores the facts it extracts as text |

[Visual memory](../vision/memory.md) stores each image caption this way.

## Expiry

An entry expires at its `expires_at`. An entry written without one takes the service's default
TTL, measured from the write; a service without a default keeps it until it is deleted.

| Step | Rule |
|------|------|
| Write | The entry's own expiry, else `now + default TTL`, else none, to the millisecond |
| Read | An entry is expired once `expires_at <= now`; `search` and `list_recent` never return one |
| Purge | `purge_expired` removes expired entries; `adk_memory::expiry::spawn_expiry_sweep` runs it on an interval |
| Delete | `delete_memory(app, user, id)` removes one entry, global or in any project |

`adk_memory::expiry` holds the rule every backend applies, and the `conformance` suite holds each
backend to it. `export_user` lists what a backend still holds, so an expired entry that no purge
has removed yet appears there with its `expires_at`.

```rust
use adk_memory::{InMemoryMemoryService, MemoryService};
use adk_memory::expiry::spawn_expiry_sweep;
use std::sync::Arc;
use std::time::Duration;

let memory: Arc<dyn MemoryService> =
    Arc::new(InMemoryMemoryService::new().with_default_ttl(Duration::from_secs(30 * 24 * 3600)));
let sweep = spawn_expiry_sweep(memory.clone(), Duration::from_secs(3600));
```

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
    async fn delete_user(&self, app: &str, user: &str) -> Result<()>;
    async fn export_user(&self, app: &str, user: &str) -> Result<Vec<UserMemory>>;
    async fn delete_session(&self, app: &str, user: &str, session: &str) -> Result<()>;
    async fn add_entry(&self, app: &str, user: &str, entry: MemoryEntry) -> Result<()>;
    async fn delete_entries(&self, app: &str, user: &str, query: &str) -> Result<u64>;
    async fn list_recent(&self, app: &str, user: &str, limit: usize) -> Result<Vec<MemoryEntry>>;
    async fn delete_memory(&self, app: &str, user: &str, memory_id: &str) -> Result<u64>;
    async fn purge_expired(&self) -> Result<u64>;
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
- **`delete_memory`** removes one entry by the id a search, listing or export returned,
  and **`purge_expired`** removes every expired entry ([Expiry](#expiry)).

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

## Erasure and export

`delete_user(app, user)` removes **all** of a user's memories (entries and
embeddings) across all projects; `export_user(app, user)` returns exactly those entries,
oldest first, each with its session and project. Every backend in this crate implements both.

```rust
use adk_memory::MemoryService;

let entries = memory.export_user("support", "alice").await?;
memory.delete_user("support", "alice").await?;
```

`MemorySubjectData` adapts any `MemoryService` to `adk_core::subject::SubjectDataStore`, so
`SubjectRights` exports and erases a person's memory with the rest of their data. See
[Subject Rights](../security/subject-rights.md).

Next: [Backends →](backends.md)
