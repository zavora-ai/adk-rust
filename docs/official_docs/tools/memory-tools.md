# Memory Tools

Memory tools enable agents to autonomously search their own long-term memory during reasoning. Instead of relying solely on external orchestration, the agent decides when and how to query memory.

## Overview

ADK-Rust provides two memory tools behind the `memory-tools` feature flag:

| Tool | Purpose | Invocation |
|------|---------|-----------|
| `LoadMemoryTool` | On-demand memory search during reasoning | Agent calls it like any other tool |
| `PreloadMemoryTool` | Auto-loads relevant context at turn start | Runs as a `BeforeModelCallback` |

## Quick Start

```rust
use adk_tool::memory::{LoadMemoryTool, PreloadMemoryTool};
use adk_memory::InMemoryMemoryService;
use std::sync::Arc;

let memory_service = Arc::new(InMemoryMemoryService::new());

// LoadMemoryTool — agent calls during reasoning
let load_tool = LoadMemoryTool::builder()
    .memory_service(memory_service.clone())
    .max_results(5)
    .min_relevance_score(0.3)
    .build()?;

// PreloadMemoryTool — auto-injects at turn start
let preload_tool = PreloadMemoryTool::builder()
    .memory_service(memory_service.clone())
    .max_results(3)
    .build()?;

// Use LoadMemoryTool as a regular tool
let agent = LlmAgentBuilder::new("assistant")
    .model(model)
    .tool(Arc::new(load_tool))
    .before_model_callback(preload_tool.into_before_model_callback())
    .build()?;
```

## Installation

```toml
[dependencies]
adk-tool = { version = "0.9.1", features = ["memory-tools"] }
adk-memory = "0.9.1"
```

## LoadMemoryTool

The agent calls this tool during reasoning to search memory with a query:

```json
{
  "name": "load_memory",
  "parameters": {
    "type": "object",
    "properties": {
      "query": { "type": "string", "description": "Search query" },
      "limit": { "type": "integer", "minimum": 1, "maximum": 100 }
    },
    "required": ["query"]
  }
}
```

The tool returns structured JSON:

```json
{
  "memories": [
    {
      "content": "The user prefers dark mode",
      "author": "assistant",
      "timestamp": "2026-05-15T10:30:00Z"
    }
  ],
  "count": 1
}
```

## PreloadMemoryTool

Can be used two ways:

### As a regular tool

The agent calls it explicitly (optional `query` parameter — falls back to user's latest input).

### As a BeforeModelCallback

Automatically injects relevant memories into the system instruction before each model call:

```rust
let preload = PreloadMemoryTool::builder()
    .memory_service(service)
    .max_results(3)
    .build()?;

let agent = LlmAgentBuilder::new("assistant")
    .model(model)
    .before_model_callback(preload.into_before_model_callback())
    .build()?;
```

## Configuration

Both tools share `MemoryToolConfig`:

| Option | Default | Range | Description |
|--------|---------|-------|-------------|
| `max_results` | 5 | 1–100 | Maximum memory entries returned |
| `min_relevance_score` | None | 0.0–1.0 | Minimum similarity threshold |
| `project_id` | None | — | Scope searches to a project |

## Project-Scoped Memory

When `project_id` is configured, searches are scoped to that project within the user's memory:

```rust
let tool = LoadMemoryTool::builder()
    .memory_service(service)
    .project_id("my-project")
    .build()?;
```

## Works With Any Backend

Memory tools delegate to the `MemoryService` trait. Any backend works:

- `InMemoryMemoryService` — development and testing
- `PostgresMemoryService` — production with pgvector
- Custom implementations

---

**Previous**: [← Schema Normalization](schema-normalization.md) | **Next**: [Sessions →](../sessions/sessions.md)
