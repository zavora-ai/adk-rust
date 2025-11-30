# adk-memory

Semantic memory and search for ADK agents.

[![Crates.io](https://img.shields.io/crates/v/adk-memory.svg)](https://crates.io/crates/adk-memory)
[![Documentation](https://docs.rs/adk-memory/badge.svg)](https://docs.rs/adk-memory)
[![License](https://img.shields.io/crates/l/adk-memory.svg)](LICENSE)

## Overview

`adk-memory` provides long-term memory capabilities for ADK agents:

- **InMemoryMemoryService** - Simple in-memory memory storage
- **Semantic Search** - Query memories by content similarity
- **Memory Entries** - Structured memory with metadata
- **Automatic Injection** - Memory context added to agent prompts

## Installation

```toml
[dependencies]
adk-memory = "0.1"
```

Or use the meta-crate:

```toml
[dependencies]
adk-rust = { version = "0.1", features = ["memory"] }
```

## Quick Start

```rust
use adk_memory::InMemoryMemoryService;
use adk_core::MemoryEntry;

// Create memory service
let service = InMemoryMemoryService::new();

// Add a memory
service.add_memory(
    "app_name",
    "user_123",
    MemoryEntry {
        content: "User prefers dark mode".to_string(),
        metadata: Default::default(),
        timestamp: Utc::now(),
    },
).await?;

// Search memories
let results = service.search_memory(
    "app_name",
    "user_123",
    "what theme does the user like?",
    5, // top_k
).await?;
```

## Memory in Agents

Memory is automatically searched when configured:

```rust
let agent = LlmAgentBuilder::new("assistant")
    .model(Arc::new(model))
    .include_memory(5) // Include top 5 relevant memories
    .build()?;
```

The runner automatically injects relevant memories into the agent's context.

## Memory Entry Structure

```rust
pub struct MemoryEntry {
    pub content: String,
    pub metadata: HashMap<String, String>,
    pub timestamp: DateTime<Utc>,
}
```

## Features

- Per-user memory isolation
- Metadata filtering
- Timestamp-based ordering
- Pluggable storage backends

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) - Meta-crate with all components
- [adk-core](https://crates.io/crates/adk-core) - Core `Memory` trait
- [adk-runner](https://crates.io/crates/adk-runner) - Memory injection during execution

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://github.com/anthropics/adk-rust) framework for building AI agents in Rust.
