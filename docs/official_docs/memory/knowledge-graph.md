# The Knowledge Graph

`GraphMemoryService` is a different shape of memory: instead of a pile of text
entries, it stores a **knowledge graph** of the user — entities, the facts
("observations") attached to them, and typed relations between them — and it
keeps a validity interval on every fact, so superseded facts become history instead of
being deleted. This is the backend behind the
[Mindfulness-with-Mia example](../realtime/examples.md#realtime_voice--mindfulness-with-mia)
and the [realtime memory page](../realtime/memory.md).

It implements the same [`MemoryService`](concepts.md#the-memoryservice-trait)
trait as the other backends, so it drops into an agent the same way — but it
exposes a richer graph API on top.

## The data model

```text
   Entity "Alice"  (type: person)
     ├─ observation: "prefers email over phone"   valid_from 2026-06-01
     ├─ observation: "timezone is CET"            valid_from 2026-06-10
     └─ relation:    Alice ──works_at──▶ "Acme"
```

- **`Entity`** — a named thing (a person, place, preference, topic) with a
  free-form `entity_type`.
- **`Observation`** — one fact attached to an entity, with a stable `id` (`i64`) and a
  `valid_from` timestamp.
- **`Relation`** — a typed edge between two entities (`source ──relation_type──▶ target`),
  e.g. `Alice ──works_at──▶ Acme`.

There's also an **episodic** store (`kg_episodic`) that logs raw turns, separate
from the curated graph — so you keep both the transcript and the distilled model.
`add_session` (the `MemoryService` method) writes there; it extracts no facts.

## Validity intervals

Every observation and relation carries `valid_from`, stamped when it is recorded, and
`valid_to`, empty while the fact is current. The crate calls this a bi-temporal graph.

When a fact changes, the old one isn't deleted — it's **invalidated** (its
`valid_to` is set) and the new one is added. Reads return only current facts, and the
superseded ones stay in the database as history.

```rust
kg.invalidate_observation(old_id).await?;   // mark a fact no longer valid (id: i64)
kg.invalidate_relation(old_id).await?;      // mark an edge no longer valid
```

`delete_entities(app, user, names)` removes entities outright, with their observations and
relations.

## Creating one

`graph-memory` is SQLite-backed, so it's a file (or `sqlite::memory:` for tests); the file is
created when missing:

```rust
use adk_memory::GraphMemoryService;
use std::sync::Arc;

let kg = GraphMemoryService::new("sqlite://mia-memory.db").await?;
kg.migrate().await?;                         // idempotent schema setup
let kg = Arc::new(kg);
```

## Writing to the graph

```rust
use adk_memory::{CreateEntityInput, CreateRelationInput};

// kg: Arc<GraphMemoryService>

kg.create_entities("coach", "alice", vec![CreateEntityInput {
    name: "Alice".into(),
    entity_type: "person".into(),
    observations: vec!["prefers morning sessions".into(), "goal: run a 10k".into()],
}]).await?;

kg.create_relations("coach", "alice", vec![CreateRelationInput {
    source: "Alice".into(), relation_type: "training_for".into(), target: "10k race".into(),
}]).await?;

// add facts to an existing entity later
kg.add_observations("coach", "alice", /* entity */ "Alice", vec!["timezone is CET".into()]).await?;
```

Creating an entity is **upsert** — re-creating a known entity updates its type and
timestamp rather than duplicating it, and adds the new observations to its existing ones.

## Reading it back

Three recall shapes, on top of the trait's `search`:

```rust
// 1. Token-scored relevance search → entities + their relations + a score
let hits = kg.search_nodes("coach", "alice", "what is she training for?", 5).await?;

// 2. Fetch specific entities by name
let nodes = kg.open_nodes("coach", "alice", vec!["Alice".into()]).await?;

// 3. The whole graph (small graphs / debugging): (Vec<Entity>, Vec<Relation>)
let (entities, relations) = kg.read_graph("coach", "alice").await?;
```

### The profile card

The killer feature for agents: `profile_card` renders a **compact, current**
summary of who the user is — the most-recently-updated entities and their valid
observations — ready to inject into a system prompt at session start.

```rust
let card = kg.profile_card("coach", "alice").await?;
// → a short text block: "Alice (person): prefers morning sessions; goal: run a 10k…"
```

Cap its size with a budget so the prompt stays small as the graph grows (the default is
12 entities and 5 observations per entity):

```rust
let kg = GraphMemoryService::new(url).await?
    .with_profile_budget(/* entities */ 12, /* observations per entity */ 5);
```

## Letting the agent curate it

You rarely call `create_entities` by hand in production — you give the **agent**
tools to write to the graph as it learns. See
[Tools & agents](tools-and-agents.md#knowledge-graph-tools) for `remember` and
`relate`, which map directly onto the calls above and ship in `adk-tool`.

Next: [Tools & agents →](tools-and-agents.md)
