# Memory

Long-term semantic memory for AI agents using `adk-memory`.

## Overview

The memory system provides persistent, searchable storage for agent conversations. Unlike session state (which is ephemeral), memory persists across sessions and enables agents to recall relevant context from past interactions.

## Installation

```toml
[dependencies]
adk-memory = "0.6.0"
```

## Core Concepts

### MemoryEntry

A single memory record with content, author, and timestamp:

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

### MemoryService Trait

The core trait for memory backends:

```rust
#[async_trait]
pub trait MemoryService: Send + Sync {
    /// Store session memories for a user
    async fn add_session(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        entries: Vec<MemoryEntry>,
    ) -> Result<()>;

    /// Search memories by query
    async fn search(&self, req: SearchRequest) -> Result<SearchResponse>;
}
```

### SearchRequest

Query parameters for memory search:

```rust
use adk_memory::SearchRequest;

let request = SearchRequest {
    query: "user preferences".to_string(),
    user_id: "user-123".to_string(),
    app_name: "my_app".to_string(),
    limit: None,
    min_score: None,
    project_id: None, // None = global only, Some("id") = global + project
};
```

## InMemoryMemoryService

Simple in-memory implementation for development and testing:

```rust
use adk_memory::{InMemoryMemoryService, MemoryService, MemoryEntry, SearchRequest};
use adk_core::Content;
use chrono::Utc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let memory = InMemoryMemoryService::new();

    // Store memories from a session
    let entries = vec![
        MemoryEntry {
            content: Content::new("user").with_text("I like Rust programming"),
            author: "user".to_string(),
            timestamp: Utc::now(),
        },
        MemoryEntry {
            content: Content::new("assistant").with_text("Rust is great for systems programming"),
            author: "assistant".to_string(),
            timestamp: Utc::now(),
        },
    ];

    memory.add_session("my_app", "user-123", "session-1", entries).await?;

    // Search memories
    let request = SearchRequest {
        query: "Rust".to_string(),
        user_id: "user-123".to_string(),
        app_name: "my_app".to_string(),
        limit: None,
        min_score: None,
        project_id: None,
    };

    let response = memory.search(request).await?;
    println!("Found {} memories", response.memories.len());

    Ok(())
}
```

## Memory Isolation

Memories are isolated by:
- **app_name**: Different applications have separate memory spaces
- **user_id**: Each user's memories are private
- **project_id** (optional): Entries can be scoped to a project within a user

```rust
// User A's memories
memory.add_session("app", "user-a", "sess-1", entries_a).await?;

// User B's memories (separate)
memory.add_session("app", "user-b", "sess-1", entries_b).await?;

// Search only returns user-a's memories
let request = SearchRequest {
    query: "topic".to_string(),
    user_id: "user-a".to_string(),
    app_name: "app".to_string(),
    limit: None,
    min_score: None,
    project_id: None, // None = global entries only
};
```

## Project-Scoped Memory

Memories can be scoped to a project within a user. The isolation key becomes `(app_name, user_id, project_id?)`:

- **Global entries** (`project_id = None`): visible in all project contexts and in global-only searches.
- **Project entries** (`project_id = Some(id)`): visible only when searching within that specific project.
- **Project search** (`project_id = Some(id)`): returns global entries + entries for that project.
- **Global search** (`project_id = None`): returns only global entries.

### Storing project-scoped entries

```rust
use adk_memory::{InMemoryMemoryService, MemoryService, MemoryEntry};
use adk_core::Content;
use chrono::Utc;

let service = InMemoryMemoryService::new();

let entry = MemoryEntry {
    content: Content::new("user").with_text("Project uses microservices"),
    author: "user".to_string(),
    timestamp: Utc::now(),
};

// Global entry (no project scope)
service.add_session("app", "user-1", "sess-1", vec![entry.clone()]).await?;

// Project-scoped entry
service.add_session_to_project("app", "user-1", "sess-2", "my-project", vec![entry.clone()]).await?;

// Single entry to a project
service.add_entry_to_project("app", "user-1", "my-project", entry).await?;
```

### Searching with project scope

```rust
use adk_memory::SearchRequest;

// Global-only search — returns only global entries
let global = service.search(SearchRequest {
    query: "microservices".into(),
    user_id: "user-1".into(),
    app_name: "app".into(),
    limit: None,
    min_score: None,
    project_id: None,
}).await?;

// Project search — returns global + project entries
let project = service.search(SearchRequest {
    query: "microservices".into(),
    user_id: "user-1".into(),
    app_name: "app".into(),
    limit: None,
    min_score: None,
    project_id: Some("my-project".into()),
}).await?;
```

### Project-scoped deletion

```rust
// Delete entries matching a query within a project only
service.delete_entries_in_project("app", "user-1", "my-project", "microservices").await?;

// Delete ALL entries for a project
service.delete_project("app", "user-1", "my-project").await?;

// Global delete — only removes global entries, project entries are unaffected
service.delete_entries("app", "user-1", "microservices").await?;

// GDPR delete_user — removes everything (global + all projects)
service.delete_user("app", "user-1").await?;
```

### MemoryServiceAdapter with project scope

The `MemoryServiceAdapter` bridges `MemoryService` to `adk_core::Memory`. Use `with_project_id()` to scope all operations:

```rust
use adk_memory::{InMemoryMemoryService, MemoryServiceAdapter};
use adk_core::Memory;
use std::sync::Arc;

let service = Arc::new(InMemoryMemoryService::new());

// Adapter without project — operates on global entries
let global_adapter = MemoryServiceAdapter::new(service.clone(), "app", "user-1");

// Adapter with project — all search/add/delete operations scoped to the project
let project_adapter = MemoryServiceAdapter::new(service.clone(), "app", "user-1")
    .with_project_id("my-project");

// Core Memory trait also supports ad-hoc project access
global_adapter.search_in_project("query", "other-project").await?;
global_adapter.add_to_project(entry, "other-project").await?;
```

### Project ID validation

Project identifiers are validated on all write operations:
- Must not be empty
- Must not exceed 256 characters

```rust
use adk_memory::validate_project_id;

validate_project_id("my-project")?;       // Ok
validate_project_id("")?;                  // Err: must not be empty
validate_project_id(&"x".repeat(257))?;   // Err: exceeds 256 chars
```

### Search semantics matrix

| `SearchRequest.project_id` | Returns Global Entries | Returns Project Entries |
|---|---|---|
| `None` | ✅ matching query | ❌ none |
| `Some("A")` | ✅ matching query | ✅ only project "A" entries matching query |

### Delete semantics matrix

| Operation | Scope |
|---|---|
| `delete_entries` (no project) | Global entries matching query only |
| `delete_entries_in_project("A")` | Project "A" entries matching query only |
| `delete_project("A")` | All entries for project "A" |
| `delete_user` | All entries (global + all projects) |

## Search Behavior

The `InMemoryMemoryService` uses word-based matching:

1. Query is tokenized into words (lowercase)
2. Each memory's content is tokenized
3. Memories with any matching words are returned

```rust
// Query: "rust programming"
// Matches memories containing "rust" OR "programming"
```

## Custom Memory Backend

Implement `MemoryService` for custom storage (e.g., vector database):

```rust
use adk_memory::{MemoryService, MemoryEntry, SearchRequest, SearchResponse};
use adk_core::Result;
use async_trait::async_trait;

pub struct VectorMemoryService {
    // Your vector DB client
}

#[async_trait]
impl MemoryService for VectorMemoryService {
    async fn add_session(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        entries: Vec<MemoryEntry>,
    ) -> Result<()> {
        // 1. Generate embeddings for each entry
        // 2. Store in vector database with metadata
        Ok(())
    }

    async fn search(&self, req: SearchRequest) -> Result<SearchResponse> {
        // 1. Generate embedding for query
        // 2. Perform similarity search
        // 3. Return top-k results
        Ok(SearchResponse { memories: vec![] })
    }
}
```

## Integration with Agents

Memory integrates with `LlmAgentBuilder`:

```rust
use adk_agent::LlmAgentBuilder;
use adk_memory::InMemoryMemoryService;
use std::sync::Arc;

let memory = Arc::new(InMemoryMemoryService::new());

let agent = LlmAgentBuilder::new("assistant")
    .model(model)
    .instruction("You are a helpful assistant with memory.")
    .memory(memory)
    .build()?;
```

When memory is configured:
1. Before each turn, relevant memories are searched
2. Matching memories are injected into the context
3. After each session, conversation is stored as memories

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                      Agent Request                          │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                    Memory Search                            │
│                                                             │
│   SearchRequest { query, user_id, app_name, project_id }   │
│                         │                                   │
│                         ▼                                   │
│   ┌─────────────────────────────────────────────────────┐  │
│   │              MemoryService                          │  │
│   │  ┌─────────┐ ┌──────────┐ ┌────────┐ ┌──────────┐  │  │
│   │  │InMemory │ │ SQLite   │ │Postgres│ │  Redis   │  │  │
│   │  │(dev)    │ │          │ │pgvector│ │          │  │  │
│   │  └─────────┘ └──────────┘ └────────┘ └──────────┘  │  │
│   │  ┌─────────┐ ┌──────────┐                           │  │
│   │  │MongoDB  │ │  Neo4j   │                           │  │
│   │  └─────────┘ └──────────┘                           │  │
│   └─────────────────────────────────────────────────────┘  │
│                         │                                   │
│                         ▼                                   │
│   SearchResponse { memories: Vec<MemoryEntry> }            │
│   (filtered by project scope)                              │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│              Context Injection                              │
│                                                             │
│   Relevant memories added to agent context                 │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                    Agent Execution                          │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                   Memory Storage                            │
│                                                             │
│   Session conversation stored for future recall            │
│   (global or project-scoped)                               │
└─────────────────────────────────────────────────────────────┘
```

## Best Practices

| Practice | Description |
|----------|-------------|
| **Use vector DB in production** | InMemory is for dev/test only |
| **Scope by user** | Always include user_id for privacy |
| **Limit results** | Cap returned memories to avoid context overflow |
| **Clean old memories** | Implement TTL or archival for stale data |
| **Embed strategically** | Store summaries, not raw conversations |

## Comparison with Sessions

| Feature | Session State | Memory |
|---------|--------------|--------|
| Persistence | Session lifetime | Permanent |
| Scope | Single session | Cross-session |
| Search | Key-value lookup | Semantic search |
| Use case | Current context | Long-term recall |

---

**Previous**: [← Guardrails](guardrails.md) | **Next**: [Studio →](../studio/studio.md)
