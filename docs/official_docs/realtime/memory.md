# Memory in Realtime Sessions

A voice that forgets you between calls feels like a kiosk. ADK-Rust lets a
realtime agent **remember** — recall what it learned about a user before it
speaks, and curate new facts mid-conversation — by wiring a `MemoryService` into
the `IntegratedRealtimeRunner`.

There are two cooperating mechanisms:

1. **Automatic context injection + turn storage** — the integration layer does
   this for you.
2. **Agent-curated memory via tools** — the agent decides what's worth keeping,
   using bridged knowledge-graph tools.

## 1. Automatic injection and storage

Attach a `MemoryService` and the runner handles the round-trip:

```rust
let runner = IntegratedRealtimeRunner::builder()
    .model(model)
    .config(config)
    .identity("support", &user_id, &session_id)
    .session_service(sessions)
    .memory_service(memory.clone())
    .integration_config(IntegrationConfig {
        inject_memory_context: true,   // query memory at connect
        max_memory_injection: 10,      // cap injected items
        store_to_memory: true,         // write each completed turn back
        persist_transcripts: true,
    })
    .build()?;
```

- **At connect**, memory is queried for the user and the results are folded into
  the session context, so the agent greets them already knowing the relevant
  history.
- **Per turn**, completed exchanges are written back (when `store_to_memory`),
  so the next session builds on this one.

Any `MemoryService` works — the in-memory one for development, or the
knowledge-graph backend below.

## 2. The knowledge-graph backend

`adk-memory`'s **`GraphMemoryService`** stores memory as a **bi-temporal
knowledge graph**: entities and relationships, each tracked along *event time*
(when it was true) and *ingestion time* (when the system learned it). That means
the agent can answer "what's the user's *current* plan?" without tripping over
the fact that it used to be something else — superseded facts stay in history
instead of overwriting the present.

```rust
use adk_memory::GraphMemoryService;
use std::sync::Arc;

let kg = Arc::new(GraphMemoryService::new(/* backing store */));
```

Pass that as the `memory_service` and the automatic injection above now draws on
the graph.

## 3. Letting the agent curate memory

The most powerful pattern: give the agent **tools** to write to the graph itself,
so it remembers deliberately rather than dumping every transcript. `adk-tool`
(feature `graph-memory-tools`) ships two that bridge straight into a realtime
session:

| Tool | What the agent does with it |
|------|-----------------------------|
| `RememberTool` (`remember`) | Store a salient fact ("prefers email over phone"). |
| `RelateTool` (`relate`) | Connect two entities ("Order A-10293 → belongs-to → this user"). |

Bridge them with `.adk_tool(...)` (see [Tools](tools.md#bridged-tools-any-adk_coretool)):

```rust
use adk_tool::{RememberTool, RelateTool};

let runner = IntegratedRealtimeRunner::builder()
    .model(model).config(config)
    .identity("support", &user_id, &session_id)
    .memory_service(kg.clone())
    .adk_tool(Arc::new(RememberTool::new(kg.clone())))
    .adk_tool(Arc::new(RelateTool::new(kg)))
    .build()?;
```

Then instruct the agent to use them:

```text
When you learn a durable fact about the customer (a preference, an account
detail, a decision), call `remember` to store it. Use `relate` to link orders
or items to the customer. Recall naturally — don't announce that you're saving.
```

Recall is **token-based**: the graph returns the entities/relationships most
relevant to the conversation, bounded by `max_memory_injection`, so context stays
small even as the graph grows.

## Choosing a depth

- **Stateless demo** → no `memory_service`. Each session starts fresh.
- **Continuity across sessions** → in-memory or `GraphMemoryService` with
  automatic injection/storage. Zero agent effort.
- **A learning agent** → `GraphMemoryService` + `remember`/`relate` tools, so the
  agent curates a clean, queryable model of the user instead of a transcript pile.

The MIA coaching example uses the graph + tools so the coach remembers the user's
goals and preferences between sessions; see [Examples](examples.md).

Next: [Building web apps →](building-web-apps.md)

## What is carried into a resumed session

`IntegratedRealtimeRunner::connect` builds one context block and prepends it to the system
instruction before the provider session is created:

| Section | Source | Bound |
|---------|--------|-------|
| `Earlier in this conversation:` | The prior session's events | `IntegrationConfig::max_history_injection` (default 20 turns) |
| `Relevant recalled context:` | `MemoryService::search` | `IntegrationConfig::max_memory_injection` (default 10 entries) |

Both are bounded because the instruction is sent once at session creation and counts against
the model's context. Each turn is rendered as `role: text`, truncated at 400 characters, and
turns without text are dropped. Setting a bound to zero disables that section.

`IntegratedRealtimeRunner::instruction()` returns what the session was created with, so the
carried context can be asserted rather than inferred:

```rust,ignore
runner.connect().await?;
let instruction = runner.instruction().await.unwrap_or_default();
assert!(instruction.contains("Relevant recalled context"));
```

> **Important:** this context was previously loaded and discarded — the prior session was
> fetched into `_session` and dropped, and the memory branch logged "injecting memory entries
> into session context" beside a comment stating that injection was a future enhancement. A
> resumed session began with neither, while the logs said otherwise.
