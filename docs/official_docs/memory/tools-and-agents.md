# Memory in Agents

Storing memory is half the story; the point is that an **agent** uses it — recalls
relevant context before it answers, and writes durable facts back as it learns.
There are two cooperating mechanisms:

1. **Attach a service** to the runner, so tools can recall through the context.
2. **Give the agent tools** so it can search and curate deliberately.

## 1. Attaching memory to an agent

A `MemoryService` is bridged to the `adk_core::Memory` interface an agent uses via
`MemoryServiceAdapter`, scoped to an `(app, user, project?)`. Attach it to the
Runner and `ToolContext::search_memory` flows through to your backend:

```rust
use adk_memory::{InMemoryMemoryService, MemoryServiceAdapter};
use adk_runner::Runner;
use std::sync::Arc;

let service = Arc::new(InMemoryMemoryService::new());
let memory: Arc<dyn adk_core::Memory> =
    Arc::new(MemoryServiceAdapter::new(service.clone(), "support", "alice"));

let runner = Runner::builder()
    .app_name("support")
    .agent(agent)
    .session_service(sessions)
    .memory_service(memory)        // ← tools can now recall through the context
    .build()?;
```

With this in place a tool can call `ctx.search_memory(query)` during a turn — but
*when* recall happens is up to how you wire it. That's what the tools are for. The
adapter is bound to one app and user; a runner that serves many users needs the tools
below, which take the app and user from each call.

## 2. Memory tools (semantic store)

`adk-tool` (feature `memory-tools`) ships two tools that turn recall into agent
behavior:

| Tool | How it runs |
|------|-------------|
| `LoadMemoryTool` | The agent calls it like any tool, when it decides it needs to recall. |
| `PreloadMemoryTool` | Runs as a `BeforeModelCallback`, auto-injecting relevant memory at the start of every turn. |

```rust
use adk_agent::LlmAgentBuilder;
use adk_tool::memory::{LoadMemoryTool, PreloadMemoryTool};
use std::sync::Arc;

// service: Arc<dyn MemoryService>
let load = LoadMemoryTool::builder()
    .memory_service(service.clone())
    .max_results(5)
    .min_relevance_score(0.3)
    .build()?;

let preload = PreloadMemoryTool::builder()
    .memory_service(service.clone())
    .max_results(3)
    .build()?;

let agent = LlmAgentBuilder::new("assistant")
    .model(model)
    .tool(Arc::new(load))                                        // on-demand recall
    .before_model_callback(preload.into_before_model_callback()) // automatic recall
    .build()?;
```

Use **preload** for "always remember the basics," **load** for "look it up when
relevant." See the full reference: [Memory Tools](../tools/memory-tools.md).

## Knowledge-graph tools

When the backend is a [`GraphMemoryService`](knowledge-graph.md), `adk-tool`
(feature `graph-memory-tools`) gives the agent two tools to **curate the graph**
itself — so it remembers deliberately instead of dumping transcripts:

| Tool | What the model does with it | Maps to |
|------|-----------------------------|---------|
| `remember` (`RememberTool`) | Save durable facts about an entity ("prefers email") | `create_entities` / `add_observations` |
| `relate` (`RelateTool`) | Record a typed relation ("Alice → works_at → Acme") | `create_relations` |

Their descriptions guide the model toward *stable, reusable* facts (names,
preferences, goals, relationships) and away from small talk. Register them
individually or as a toolset:

```rust
use adk_agent::LlmAgentBuilder;
use adk_memory::GraphMemoryService;
use adk_tool::memory::{GraphMemoryToolset, RelateTool, RememberTool};
use std::sync::Arc;

let kg = Arc::new(GraphMemoryService::new("sqlite://mem.db").await?);
kg.migrate().await?;

// individually…
let agent = LlmAgentBuilder::new("coach")
    .model(model)
    .tool(Arc::new(RememberTool::new(kg.clone())))
    .tool(Arc::new(RelateTool::new(kg.clone())))
    .build()?;

// …or as one toolset
let toolset = GraphMemoryToolset::new(kg.clone());
```

Pair this with the graph's [`profile_card`](knowledge-graph.md#the-profile-card)
injected at session start, and you get an agent that **reads** who the user is up
front and **writes** what it learns back — the loop that makes memory feel real.

## In realtime sessions

The same wiring works for voice/multimodal agents through
`IntegratedRealtimeRunner` — the agent's tools, `remember`/`relate` included, are bridged
into the realtime session, and its `memory_service` injects context at connect and stores
completed turns. See
[Realtime → Memory](../realtime/memory.md).

## A complete pattern

The [`realtime_voice` (Mindfulness with Mia)](../realtime/examples.md#realtime_voice--mindfulness-with-mia)
example is the end-to-end reference: a file-backed `GraphMemoryService` is Mia's
long-term memory; her profile card is injected at session start; she curates
facts mid-conversation via `remember`/`relate`; and a live panel reads and writes
the same graph. Read it to see every piece on this page working together.

← Back to the [Memory overview](index.md)
