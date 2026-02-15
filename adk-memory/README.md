# adk-memory

Semantic memory and search for Rust Agent Development Kit (ADK-Rust) agents.

[![Crates.io](https://img.shields.io/crates/v/adk-memory.svg)](https://crates.io/crates/adk-memory)
[![Documentation](https://docs.rs/adk-memory/badge.svg)](https://docs.rs/adk-memory)
[![License](https://img.shields.io/crates/l/adk-memory.svg)](LICENSE)

## Overview

`adk-memory` provides long-term memory capabilities for the Rust Agent Development Kit ([ADK-Rust](https://github.com/zavora-ai/adk-rust)):

- **InMemoryMemoryService** - Simple in-memory memory storage
- **MemoryService** - Trait for custom storage backends
- **Semantic Search** - Query memories by content similarity
- **Memory Entries** - Structured memory with content and metadata

## Installation

```toml
[dependencies]
adk-memory = "0.3.1"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "0.3.1", features = ["memory"] }
```

## Quick Start

```rust
use adk_memory::{InMemoryMemoryService, MemoryService, MemoryEntry, SearchRequest};
use adk_core::Content;
use chrono::Utc;

// Create memory service
let service = InMemoryMemoryService::new();

// Add memories from a session
let entries = vec![
    MemoryEntry {
        content: Content::new("user").with_text("User prefers dark mode"),
        author: "system".to_string(),
        timestamp: Utc::now(),
    },
];

service.add_session(
    "my_app",
    "user_123",
    "session_456",
    entries,
).await?;

// Search memories
let response = service.search(SearchRequest {
    query: "what theme does the user like?".to_string(),
    user_id: "user_123".to_string(),
    app_name: "my_app".to_string(),
}).await?;

for memory in response.memories {
    println!("Found: {:?}", memory.content);
}
```

## Memory Entry Structure

```rust
pub struct MemoryEntry {
    pub content: Content,           // Message content with parts
    pub author: String,             // Who created this memory
    pub timestamp: DateTime<Utc>,   // When it was created
}
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

## Features

- Per-user memory isolation
- Simple keyword-based search
- Timestamp tracking
- Pluggable storage backends

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-core](https://crates.io/crates/adk-core) - Core `Memory` trait
- [adk-runner](https://crates.io/crates/adk-runner) - Memory injection during execution

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
