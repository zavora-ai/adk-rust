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

The backends live in `adk-graph`, which does not depend on `adk-session`. Their database
drivers are the versions `adk-session` uses, so an application with both holds one copy
of each driver.

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

Pauses and failures are written before the run returns in every mode: a step that fails is
always checkpointed with its record, so an observer sees that the run failed.

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

## Run and step records

Every checkpoint a run writes carries what happened since the previous one, so a
workflow observer reads a run's history from its checkpoints alone — after a restart,
and for runs any host started.

| Metadata key | Reader | Content |
|--------------|--------|---------|
| `adk.graph.run` | `Checkpoint::run_record()` | `RunRecord { runId, startedAt }` |
| `adk.graph.steps` | `Checkpoint::step_records()` | The `StepRecord`s finished since the previous saved checkpoint |
| `adk.graph.lineage` | `Checkpoint::lineage()` | A forked thread's `Lineage`; absent on a thread that was not forked |

A checkpoint written before these records existed reads as empty: no run, no steps, no
lineage. Under `Durability::Exit` a checkpoint holds every step since the previous one:
the whole run, or the steps before a non-idempotent or governed node's step, which is
flushed first. Such a checkpoint holds the state after its last step only, so an observer
opens and forks only that step. `CompiledGraph::update_state` carries all three keys to
the checkpoint it writes.

**Runs.** The run id is the invocation that started the thread from its entry nodes: the
parent invocation's id when a `GraphAgent` runs under a `Runner`, otherwise a new id. A
run continues across pauses, approval turns (an approval resumes in a new invocation)
and resumes. A finished thread that a `GraphAgent` turn starts again from its entry opens
a new run, and so does the first run of a fork.

**Steps.** A `StepRecord { step, kind, nodes }` has `kind` `completed`, `paused` or
`failed`, and one `NodeRecord` per node of the step:

| Field | Content |
|-------|---------|
| `node` | The node's name |
| `startedAt`, `durationMs` | When the node started and how long it ran, measured per node, so parallel nodes have their own times |
| `status` | `done`, `paused`, `failed`, `recorded`, `refused` or `notRun` |
| `writes` | The state keys the node wrote, sorted |
| `attempt` | The node's attempt count when it finished |
| `effectKey` | A governed node's action-ledger key |
| `error` | The error a failed or refused node ended with |

`recorded` and `refused` belong to governed nodes; see
[Graph Agents](graph-agents.md#governed-nodes). `notRun` is a node that did not start
because a sibling failed first, or a governed node with no approval gate.

**Checkpoint ids, not step numbers.** `step` is a label: the `Checkpoint::step` of the
completed checkpoint the step produces. A pause checkpoint has the same
`Checkpoint::step` as the completed checkpoint before it, so address checkpoints by id.
`Checkpoint::is_paused()` tells a pause apart: a node of its frontier waits mid-step. A run
that stops at an `interrupt_after` gate records its finished step as `paused` with no node
waiting; the step that resumes it takes the next label, so after such a gate the labels run
ahead of `Checkpoint::step`.

Node lifecycle is recorded here, durably, and is not forwarded to the runner's event
stream.

```rust
use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer};
use adk_graph::edge::{END, START};
use adk_graph::graph::StateGraph;
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::record::{NodeStatus, StepKind};
use adk_graph::state::State;
use serde_json::json;
use std::sync::Arc;

async fn run() -> adk_graph::Result<()> {
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let graph = StateGraph::with_channels(&["invoice"])
        .add_node_fn("draft", |_ctx| async {
            Ok(NodeOutput::new().with_update("invoice", json!({ "amountCents": 4200 })))
        })
        .add_edge(START, "draft")
        .add_edge("draft", END)
        .compile()?
        .with_checkpointer_arc(checkpointer.clone());
    graph.invoke(State::new(), ExecutionConfig::new("invoice-7")).await?;

    for checkpoint in checkpointer.list("invoice-7").await? {
        for step in checkpoint.step_records()? {
            assert_eq!(step.kind, StepKind::Completed);
            assert_eq!(step.nodes[0].status, NodeStatus::Done);
        }
    }
    Ok(())
}
```

## Forking with edits

With the `time-travel` feature, `TimeTravelHandle::fork` and `time_travel::fork_thread`
branch a new thread from a checkpoint, with channels overwritten. The source thread is
never written.

```rust
use adk_graph::checkpoint::{Checkpointer, MemoryCheckpointer};
use adk_graph::edge::{END, START};
use adk_graph::graph::StateGraph;
use adk_graph::node::{ExecutionConfig, NodeOutput};
use adk_graph::state::State;
use adk_graph::time_travel::ForkSpec;
use serde_json::json;
use std::sync::Arc;

async fn fork() -> Result<(), Box<dyn std::error::Error>> {
    let checkpointer = Arc::new(MemoryCheckpointer::new());
    let graph = StateGraph::with_channels(&["note", "sent"])
        .add_node_fn("draft", |_ctx| async { Ok(NodeOutput::new().with_update("note", json!("v1"))) })
        .add_node_fn("send", |_ctx| async { Ok(NodeOutput::new().with_update("sent", json!(true))) })
        .add_edge(START, "draft")
        .add_edge("draft", "send")
        .add_edge("send", END)
        .compile()?
        .with_checkpointer_arc(checkpointer.clone());
    graph.invoke(State::new(), ExecutionConfig::new("t1")).await?;

    // The completed checkpoint after `draft`: `send` runs again in the fork.
    let after_draft = checkpointer.list("t1").await?[0].checkpoint_id.clone();
    let spec = ForkSpec::new(&after_draft, "t1-fork").set("note", json!("re-run for audit"));
    graph.time_travel("t1")?.fork(spec).await?;
    graph.invoke(State::new(), ExecutionConfig::new("t1-fork")).await?;
    Ok(())
}
```

| Rule | Behaviour |
|------|-----------|
| Source | The completed checkpoint of a step, by id. An unknown id, a pause (`fork.paused`) and a checkpoint with no pending nodes (`fork.nothing_to_run`, "nothing runs after the last step") are refused |
| Target | A new thread; one that already has checkpoints is refused (`fork.thread_exists`). `TimeTravelHandle::fork` also refuses a checkpoint of another thread |
| Edits | Overwrite whole channels; reducers are not applied |
| Checked edits | With the graph's schema (`TimeTravelHandle::fork`, or `fork_thread` with `Some(schema)`), each key must be a declared channel (`fork.undeclared_channel`) |
| Unchecked edits | Without a schema, only keys the source state holds are accepted (`fork.unknown_key`), and the lineage records `checked: false` |
| Carried | State with the edits, step, pending nodes, the arrivals of joins still waiting, and each governed node's completed runs |
| Dropped | The retry budget, the answered gate, pending writes, finished children, the paused marker, the run and its step records; a pause asks again in the fork |

`ForkError::code` returns the stable code each refusal's message starts with.

The fork records a `Lineage { root, parent, parentCheckpoint, parentStep, edited,
checked }`, which every later checkpoint of the fork carries. `root` is the source's
root, or the source thread when the source was not forked, so a fork of a fork keeps the
thread the family started from. `fork_at` records lineage the same way, with no edits.

> **Note:** `fork_at` used to drop the arrivals of a join still waiting, so a fork at
> such a step fired the join with only the arrivals after the fork. Forks now keep them.

## The effect log

A governed node's executed call appends `{ key, node, writes, goto, thread, checkpoint,
at }` to the effect-log thread `{lineage root}:effects`
(`governed::effect_log_thread`). A later run in the same lineage whose call is unchanged
is answered from the action ledger and this log, without running: its writes are the
original call's. Read the log with `governed::effect_log(checkpointer, root)`.

| Property | Behaviour |
|----------|-----------|
| Writer | Only the governed bracket; forks never write it |
| Readers | Every thread of the lineage, so sibling forks find effects another fork ran |
| Missing writes | A completed call whose writes are missing fails closed with `graph.effect_unavailable` and is not run |

## Records and retention

`RetentionPolicy` prunes the thread a run writes, never the effect log, so a pruned
thread's forks still find the effects it ran. Pruning removes old step records with
their checkpoints; the governed nodes' completed-run counts travel on every checkpoint,
so pruning never makes a later run of a loop reuse an earlier run's record.

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
