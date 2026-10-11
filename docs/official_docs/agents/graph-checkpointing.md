# Graph Checkpointing

A graph run writes a checkpoint after each super-step, so it resumes where it stopped —
after a pause, a failure, or a process restart, on this process or another. A
`Checkpointer` stores those checkpoints. Every backend implements the same trait and
passes the same conformance suite, so choosing a backend changes one constructor and
nothing else.

## Backend matrix

| Backend | `adk-graph` feature | `adk-rust` feature | Type | Constructors | Conformance run |
|---------|---------------------|--------------------|------|--------------|-----------------|
| In memory | — | `graph` | `MemoryCheckpointer` | `new()` | Every PR; restart cases see one process |
| SQLite | `sqlite` | `graph-sqlite` | `SqliteCheckpointer` | `new(url)`, `from_pool(pool)`, `in_memory()` | Every PR |
| PostgreSQL | `postgres` | `graph-postgres` | `PostgresCheckpointer` | `new(url)`, `from_pool(pool)` | Nightly, live server |
| Redis | `redis` | `graph-redis` | `RedisCheckpointer` | `new(url)`, `from_client(client)` | Nightly, live server |
| MongoDB | `mongodb` | `graph-mongodb` | `MongoCheckpointer` | `new(url, database)`, `from_database(db)` | Nightly, live server |
| Neo4j | `neo4j` | `graph-neo4j` | `Neo4jCheckpointer` | `new(uri, user, password)`, `from_graph(graph)` | Nightly, live server |
| Firestore | `firestore` | `graph-firestore` | `FirestoreCheckpointer` | `new(project)`, `from_db(db)`, `connect_emulator(project)` | Nightly, emulator |

`DeltaCheckpointer` wraps any backend and stores each step as a diff of the previous
one. It loads full checkpoints but lists the stored diffs, so it is not run against
the suite's round-trip cases.

The backends live in `adk-graph` rather than `adk-session`: `adk-graph` depends only on
`adk-core`, and putting them in `adk-session` would add an `adk-session → adk-graph`
edge and move `adk-session` and every crate above it one publish tier later. Driver
versions match `adk-session`, so the lockfile holds one copy of each driver.

## Setup

1. Enable the backend's feature.
2. Construct it with `new(...)`, or from a handle a session service already holds.
3. Call `migrate()` once at startup (the shared-database backends). It creates
   tables, indexes, and constraints, or loads scripts, and is safe to call from
   several processes at once.
4. Attach it with `CompiledGraph::with_checkpointer` or `with_checkpointer_arc`.

```toml
[dependencies]
adk-graph = { version = "3.0.0", features = ["postgres"] }
```

```rust
use adk_graph::PostgresCheckpointer;
use adk_graph::edge::{END, START};
use adk_graph::graph::StateGraph;
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::State;
use serde_json::json;

async fn run() -> adk_graph::Result<()> {
    let checkpointer = PostgresCheckpointer::new("postgres://localhost/app").await?;
    checkpointer.migrate().await?;

    let graph = StateGraph::with_channels(&["result"])
        .add_node_fn("work", |_ctx| async { Ok(NodeOutput::new().with_update("result", json!("done"))) })
        .add_edge(START, "work")
        .add_edge("work", END)
        .compile()?
        .with_checkpointer(checkpointer);

    graph.invoke(State::new(), ExecutionConfig::new("order-42")).await?;
    Ok(())
}
```

## The contract

| Rule | Behaviour |
|------|-----------|
| Ordering | A thread's checkpoints are ordered by `Checkpoint::version`, never by time. `load` returns the highest version; `list` ascends |
| Monotonic versions | `save` stores a checkpoint only when its version is at least 1 and greater than every version stored for the thread; otherwise it fails with `GraphError::CheckpointConflict` (`graph.checkpoint.conflict`) |
| Unique IDs | A checkpoint ID is stored once; a second save with a stored ID fails |
| Paging | `list_page(thread, before, limit)` returns at most `limit` checkpoints, newest first, below version `before` |
| Thread isolation | `delete` and `prune` touch only the named thread |
| Retention | `prune(thread, policy)` removes what a `RetentionPolicy` selects and never the newest checkpoint |
| Fidelity | A loaded checkpoint equals the saved one, `created_at` to the microsecond |

### Concurrent writers

The version rule is what keeps two writers on one thread from interleaving. A writer
continues a thread from its newest checkpoint, version `n`, and saves `n + 1`. A second
writer that read the same checkpoint also saves `n + 1` and is refused:

| Backend | How the rule is enforced |
|---------|--------------------------|
| Memory | One lock over the store |
| SQLite, PostgreSQL | A guarded `INSERT` and a unique index on `(thread_id, version)` |
| Redis | One Lua script per save reads the thread's highest version and writes |
| MongoDB | A unique index on `(thread_id, version)` and a conditional upsert of the thread's head |
| Neo4j | A write lock on the thread's node, taken before the head is read |
| Firestore | A serializable transaction over the thread's head document |

A graph run that is refused stops with `GraphError::CheckpointConflict` and stores
nothing more. Run one executor per thread; a run queue with leases is the place to
guarantee that, and the conflict is the backstop when it fails.

`update_state`, `fork_at`, and the functional API continue from the newest version
the same way, so editing a paused thread while it resumes elsewhere is refused rather
than lost.

## Durability modes

`ExecutionConfig::with_durability` sets when a run writes each super-step's checkpoint:

| Mode | Checkpoint written | A crash loses |
|------|--------------------|---------------|
| `Durability::Sync` (default) | Before the next super-step starts | Nothing that completed |
| `Durability::Async` | In the background while the next super-step runs | At most the last completed super-step |
| `Durability::Exit` | When the run completes, pauses, or fails | Everything since the run started |

Pauses and failures are written before the run returns in every mode.

A super-step that runs a node declared non-idempotent first writes every earlier
checkpoint, as `Sync` does, so a crash during the side effect cannot resume from before
an earlier step and repeat it. Declare the effect on the compiled graph; a node that
declares nothing is not treated as having an effect:

```rust
use adk_core::ToolEffect;
use adk_graph::checkpoint::Durability;
use adk_graph::edge::{END, START};
use adk_graph::graph::StateGraph;
use adk_graph::node::{ExecutionConfig, NodeOutput};

fn build() -> adk_graph::Result<()> {
    let graph = StateGraph::with_channels(&["receipt"])
        .add_node_fn("charge", |_ctx| async { Ok(NodeOutput::new()) })
        .add_edge(START, "charge")
        .add_edge("charge", END)
        .compile()?
        .with_node_effect("charge", ToolEffect::NonIdempotent);
    let config = ExecutionConfig::new("order-42").with_durability(Durability::Async);
    let _ = (graph, config);
    Ok(())
}
```

## Pending writes

When one node of a parallel super-step fails, the nodes that succeeded keep their
outputs as `Checkpoint::pending_writes`, and the step is saved without being applied.
A resumed run applies those outputs instead of running the finished nodes again, and
runs only the failed node:

| Super-step | Saved | On resume |
|------------|-------|-----------|
| Completes | State with every update applied; no pending writes | The next step runs |
| A node fails | The pre-step state, the same frontier, and each finished node's updates and routing | Finished nodes are answered from their pending writes; the failed node runs |
| The process crashes mid-step | Nothing new | Every node of the step runs again |

A node with an external side effect still needs an idempotency key: a crash between
the side effect and the checkpoint repeats the node.

## Running the conformance suite

Enable the `conformance` feature in `[dev-dependencies]` and implement
`CheckpointerHarness`. Each call to `open` returns a new handle over the same data, as
a restarted process would see it:

```rust
use std::sync::Arc;

use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer};
use adk_graph::conformance::CheckpointerHarness;

#[derive(Default)]
struct InMemory(Arc<MemoryCheckpointer>);

#[async_trait::async_trait]
impl CheckpointerHarness for InMemory {
    async fn open(&self) -> Arc<dyn Checkpointer> {
        self.0.clone()
    }
}

adk_graph::checkpointer_conformance_tests!(harness = InMemory::default());
```

The macro generates one test per case: round trip, version ordering, paging, load by
ID, delete, prune policies, thread isolation, stale writes, concurrent writers across
two handles, restart survival, a 512 KiB state, and identifier escaping. Attributes
before `harness =` apply to every test, so a live-server suite is marked `#[ignore]`.

To run the shipped live suites locally:

```bash
DATABASE_URL=postgres://postgres@localhost:5432/postgres \
REDIS_URL=redis://localhost:6379 \
  cargo nextest run -p adk-graph --features postgres,redis --run-ignored only \
  --test checkpointer_conformance_postgres --test checkpointer_conformance_redis
```
