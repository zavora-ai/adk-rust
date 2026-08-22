# Knowledge-Graph Memory for a Text Agent

A plain **text** `LlmAgent` — no realtime, no voice — given a **bi-temporal
knowledge graph** as its long-term memory, using the reusable `adk-tool`
built-ins. It shows the general pattern any agent uses to gain durable,
structured memory:

| Tool | From | Role |
|------|------|------|
| `remember` / `relate` | `adk_tool::GraphMemoryToolset` (feature `graph-memory-tools`) | **Write** — create entities/observations and typed relations |
| `load_memory` | `adk_tool::memory::LoadMemoryTool` | **Recall** — search memory; works over any `MemoryService` |

All three are backed by one shared
[`GraphMemoryService`](../../adk-memory/src/graph.rs). Nothing here is bespoke to
a voice/realtime stack — swap in any model and you have a text agent with KG
memory.

## What it demonstrates

1. **Session 1** — the agent meets *Alex*. As Alex shares durable facts, the
   model calls `remember`/`relate` to write them into the graph.
2. The program prints the resulting graph — entities, observations, relations.
3. **Session 2** — a brand-new session with its **own** `SessionService`, so it
   shares *no* chat history with session 1. It still knows Alex: the profile
   card is injected into its instruction, and `load_memory` recalls the rest —
   all from the shared graph.

## Run

```bash
export GOOGLE_API_KEY=...   # or GEMINI_API_KEY
cargo run --manifest-path examples/knowledge_graph_agent/Cargo.toml
```

Override the model with `GEMINI_MODEL` (default `gemini-2.5-flash`). With no key
set, the example prints guidance and exits cleanly.

## Sample run (abridged)

```text
Session 1 — the agent meets Alex and learns about them

👤 Hi! I'm Alex. I'm vegetarian and severely allergic to peanuts.
🤖   🛠  remember({"entity":"Alex","entity_type":"person","facts":["is vegetarian","is severely allergic to peanuts"]})
   It's nice to meet you, Alex. I'll remember that.

👤 I work at Acme Corp as a data engineer, and I'm training for a marathon in April.
🤖   🛠  remember({"entity":"Alex","facts":["works as a data engineer","is training for a marathon in April"]})
     🛠  relate({"source":"Alex","relation":"works_at","target":"Acme Corp"})

Knowledge graph after session 1
   • Alex [person]
       – is vegetarian
       – is severely allergic to peanuts
       – works as a data engineer
       – is training for a marathon in April
   relations:
       Alex —works_at→ Acme Corp

Session 2 — a brand-new session (no shared chat history) still knows Alex

👤 I'm ordering dinner — suggest a dish for me, keeping my needs in mind.
🤖   🛠  load_memory({"query":"what does Alex eat"})
   Considering you're vegetarian with a severe peanut allergy, a lentil soup or
   a peanut-free vegetable curry would be a safe, delicious option.
```

## How it works

```text
        ┌──────────────────────────── shared GraphMemoryService ───────────────────────────┐
        │                                                                                   │
Session 1 (its own SessionService)                         Session 2 (its own SessionService)
  LlmAgent  ──remember/relate──▶  ✍ writes entities,         LlmAgent  ◀── profile card injected
            (writes facts)         observations, relations             ◀── load_memory recalls
```

- **Writes** go through `GraphMemoryToolset` (`remember`, `relate`), scoped to
  the invocation's `(app_name, user_id)` via the `ToolContext`.
- **Recall** uses `LoadMemoryTool`, which calls `MemoryService::search`. For the
  graph that returns the **profile card** for a generic/empty query, or
  token-matched facts (plus episodic turns) for a topic query.
- **Cross-session continuity** comes entirely from the graph: each session has a
  fresh `SessionService` (no shared transcript), yet session 2 answers grounded
  in what session 1 stored.

The graph here is in-memory (`sqlite::memory:`) and shared across both sessions
in one process. Point `GraphMemoryService::new` at a file (`sqlite:kg.db`) to
have the agent remember the user across process restarts.

## Feature flags

```toml
adk-memory = { version = "2.1.0", features = ["graph-memory"] }
adk-tool   = { version = "2.1.0", features = ["graph-memory-tools"] }
```
