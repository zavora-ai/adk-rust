# Memory Tools

Memory tools enable agents to autonomously search their own long-term memory during reasoning. Instead of relying solely on external orchestration, the agent decides when and how to query memory.

## Overview

ADK-Rust provides two memory tools behind the `memory-tools` feature flag:

| Tool | Purpose | Invocation |
|------|---------|-----------|
| `LoadMemoryTool` | On-demand memory search during reasoning | Agent calls it like any other tool |
| `PreloadMemoryTool` | Loads memories related to the user's latest message | As a tool, or as a `BeforeModelCallback` before every model call |

Both are read-only and concurrency-safe (`ToolEffect::ReadOnly`), so a policy's
`allow_read_only()` admits them and `Auto` dispatch can run them concurrently.

## Quick Start

```rust
use adk_agent::LlmAgentBuilder;
use adk_memory::InMemoryMemoryService;
use adk_tool::memory::{LoadMemoryTool, PreloadMemoryTool};
use std::sync::Arc;

let memory_service = Arc::new(InMemoryMemoryService::new());

// LoadMemoryTool — agent calls during reasoning
let load_tool = LoadMemoryTool::builder()
    .memory_service(memory_service.clone())
    .max_results(5)
    .min_relevance_score(0.3)
    .build()?;

// PreloadMemoryTool — auto-injects before each model call
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
adk-tool = { version = "3.0.0", features = ["memory-tools"] }
adk-memory = "3.0.0"
```

Through the umbrella crate, enable the `memory-tools` feature of `adk-rust`.

## LoadMemoryTool

The agent calls this tool during reasoning to search memory with a query:

```json
{
  "name": "load_memory",
  "parameters": {
    "type": "object",
    "properties": {
      "query": { "type": "string", "description": "The search query to find relevant memories" },
      "limit": { "type": "integer", "description": "Maximum number of results to return", "minimum": 1, "maximum": 100 }
    },
    "required": ["query"]
  }
}
```

A `limit` above the configured `max_results` is capped at `max_results`. The search runs
for the calling user and app. The tool returns structured JSON, with each memory's text
parts joined by spaces and its timestamp in RFC 3339:

```json
{
  "memories": [
    {
      "content": "The user prefers dark mode",
      "author": "assistant",
      "timestamp": "2026-05-15T10:30:00+00:00"
    }
  ],
  "count": 1
}
```

## PreloadMemoryTool

Can be used two ways:

### As a regular tool

The agent calls `preload_memory` with an optional `query`; without one, it searches with the
text of the user's latest input, and returns the same JSON as `load_memory`.

### As a BeforeModelCallback

Before each model call, the callback searches with the text of the request's last `user`
message and, when memories match, adds them as text to the request's `system` content —
appended to an existing `system` entry, or inserted as the first content:

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

Both builders take the options of `MemoryToolConfig`; `build()` fails when an option is out
of range:

| Option | Default | Range | Description |
|--------|---------|-------|-------------|
| `max_results` | 5 | 1–100 | Maximum memory entries returned |
| `min_relevance_score` | None | 0.0–1.0 | Minimum similarity threshold |
| `project_id` | None | — | Scope searches to a project |

`build()` also fails when no `memory_service` was set.

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
- `SqliteMemoryService`, `PostgresMemoryService`, `RedisMemoryService`, `MongoMemoryService`, `Neo4jMemoryService`, `VertexAiMemoryBankService` — persistent backends behind their `adk-memory` features; see [Memory backends](../memory/backends.md)
- Custom implementations

The `graph-memory-tools` feature adds `RememberTool`, `RelateTool`, and `GraphMemoryToolset`
for knowledge-graph memory curation.

---

**Previous**: [← Schema Normalization](schema-normalization.md) | **Next**: [Sessions →](../sessions/sessions.md)
