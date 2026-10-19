# Memory in Realtime Sessions

A `MemoryService` attached to the `IntegratedRealtimeRunner` lets a realtime agent recall what
it learned about a user before it speaks, and record new facts during the conversation.

There are two cooperating mechanisms:

1. **Context injection and turn storage**, done by the integration layer.
2. **Agent-curated memory through tools**: the agent decides what to keep, using bridged
   knowledge-graph tools.

## 1. Injection and storage

Attach a `MemoryService` and the runner handles the round-trip:

```rust
use adk_memory::MemoryService;
use adk_realtime::config::RealtimeConfig;
use adk_realtime::integration::{IntegratedRealtimeRunner, IntegrationConfig};
use adk_realtime::model::BoxedModel;
use adk_session::SessionService;
use std::sync::Arc;

fn build(
    model: BoxedModel,
    config: RealtimeConfig,
    sessions: Arc<dyn SessionService>,
    memory: Arc<dyn MemoryService>,
    user_id: &str,
    session_id: &str,
) -> adk_realtime::Result<IntegratedRealtimeRunner> {
    IntegratedRealtimeRunner::builder()
        .model(model)
        .config(config)
        .identity("support", user_id, session_id)
        .session_service(sessions)
        .memory_service(memory)
        .integration_config(IntegrationConfig {
            inject_memory_context: true, // query memory at connect
            max_memory_injection: 10,    // cap recalled entries
            store_to_memory: true,       // write each completed turn back
            persist_transcripts: true,
            max_history_injection: 20,   // cap prior turns carried over
        })
        .build()
}
```

- **At connect**, the runner searches memory for the user with the query `"session context"`
  and carries the results into the session instruction (see
  [What is carried into a resumed session](#what-is-carried-into-a-resumed-session)).
- **Per turn**, completed turns are written back with `MemoryService::add_session` when
  `store_to_memory` is set, so the next session builds on this one.

Any `MemoryService` works: the in-memory one for development, or the knowledge-graph backend
below.

## 2. The knowledge-graph backend

`adk-memory`'s `GraphMemoryService` (feature `graph-memory`) stores memory as a bi-temporal
knowledge graph in SQLite: entities, typed relations, and observations. Observations and
relations carry `valid_from` and an optional `valid_to`; superseding a fact stamps `valid_to`
instead of deleting it, so history is kept and queries return only current facts.

```rust
use adk_memory::GraphMemoryService;
use std::sync::Arc;

async fn knowledge_graph() -> anyhow::Result<Arc<GraphMemoryService>> {
    let kg = GraphMemoryService::new("sqlite:memory.db").await?;
    kg.migrate().await?;
    Ok(Arc::new(kg))
}
```

As the runner's `memory_service`, it answers the session-start query with a compact profile
card of the user's current facts, and any other query with matching facts and turns.

## 3. Letting the agent curate memory

`adk-tool` (feature `graph-memory-tools`) has two tools that write to the graph and bridge
into a realtime session:

| Tool | What the agent does with it |
|------|-----------------------------|
| `RememberTool` (`remember`) | Store a salient fact ("prefers email over phone") |
| `RelateTool` (`relate`) | Connect two entities ("Order A-10293 → belongs-to → this user") |

Bridge them with `.adk_tool(...)` (see [Tools](tools.md#bridged-tools-any-adk_coretool)):

```rust
use adk_memory::GraphMemoryService;
use adk_realtime::config::RealtimeConfig;
use adk_realtime::integration::IntegratedRealtimeRunner;
use adk_realtime::model::BoxedModel;
use adk_tool::{RelateTool, RememberTool};
use std::sync::Arc;

fn build(
    model: BoxedModel,
    config: RealtimeConfig,
    kg: Arc<GraphMemoryService>,
    user_id: &str,
    session_id: &str,
) -> adk_realtime::Result<IntegratedRealtimeRunner> {
    IntegratedRealtimeRunner::builder()
        .model(model)
        .config(config)
        .identity("support", user_id, session_id)
        .memory_service(kg.clone())
        .adk_tool(Arc::new(RememberTool::new(kg.clone())))
        .adk_tool(Arc::new(RelateTool::new(kg)))
        .build()
}
```

Then instruct the agent to use them:

```text
When you learn a durable fact about the customer (a preference, an account
detail, a decision), call `remember` to store it. Use `relate` to link orders
or items to the customer. Recall naturally — don't announce that you're saving.
```

## Choosing a depth

| Need | Setup |
|------|-------|
| Each session starts fresh | No `memory_service` |
| Continuity across sessions | An in-memory service or `GraphMemoryService`, with injection and storage |
| An agent that curates what it knows | `GraphMemoryService` with the `remember` and `relate` tools |

The `realtime_voice` example ("Mindfulness with Mia") uses the graph and both tools; see
[Examples](examples.md#realtime_voice--mindfulness-with-mia).

## What is carried into a resumed session

`IntegratedRealtimeRunner::connect` builds one context block and prepends it to the system
instruction before the provider session is created:

| Section | Source | Bound |
|---------|--------|-------|
| `Earlier in this conversation:` | The prior session's events | `IntegrationConfig::max_history_injection` (default 20 turns) |
| `Relevant recalled context:` | `MemoryService::search` | `IntegrationConfig::max_memory_injection` (default 10 entries) |

Both are bounded because the instruction is sent once at session creation and counts against
the model's context. The most recent turns are kept, each rendered as `role: text` and cut at
400 bytes; turns without text are dropped. Setting a bound to zero disables that section. A failed
session load or memory search is logged and the session connects without that section.

`IntegratedRealtimeRunner::instruction()` returns what the session was created with, so the
carried context can be checked:

```rust
use adk_realtime::integration::IntegratedRealtimeRunner;

async fn carried_memory(runner: &IntegratedRealtimeRunner) -> adk_realtime::Result<bool> {
    runner.connect().await?;
    let instruction = runner.instruction().await.unwrap_or_default();
    Ok(instruction.contains("Relevant recalled context"))
}
```

Next: [Building web apps →](building-web-apps.md)
