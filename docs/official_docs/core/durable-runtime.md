# Durable Runtime

Four store contracts in `adk-core` let agent runs outlive a process and run safely across
replicas: a lease store, a run store, a schedule store, and a governance state store. Each is
a backend-neutral trait with an in-memory implementation and a conformance suite that every
backend passes.

| Contract | Trait | In-memory implementation | Holds |
|----------|-------|--------------------------|-------|
| Leases | `LeaseStore` | `InMemoryLeaseStore` | One owner per named resource, with a fencing epoch |
| Runs | `DurableRunStore` | `InMemoryRunStore` | The run queue: input, state, attempts, lease, checkpoint, outcome |
| Schedules | `ScheduleStore` | `InMemoryScheduleStore` | Cron, one-shot, and interval schedules with their next fire time |
| Governance state | `GovernanceStateStore` | `InMemoryGovernanceStateStore` | Freeze state per organisation, tenant, and agent |

The run store is named `DurableRunStore` after the role it plays: the durable queue every
replica claims from. `adk-server`'s background runs, cron jobs, workers, and scheduler are built
on these contracts (see [Worker](#worker) and [Scheduler](#scheduler)).

Time comes from the injected `adk_core::Clock`: `with_clock` sets it on every in-memory store
that reads the time, and the conformance suites drive a `ManualClock`.

## Design rules

| Rule | Meaning |
|------|---------|
| One transactional store holds durable truth | Caches and notification channels only shorten the time until a replica reads the store |
| Fencing | Every worker write carries its lease epoch; a stale epoch is refused |
| Revisions decide conflicts | Exact revision or epoch markers, never timestamps, decide which concurrent write wins |
| Resume never repeats a side effect | A recovered run resumes from its checkpoint; non-idempotent tool calls go through the [action ledger](../security/durable-stores.md) |
| Fail loudly on unknown ids | Operations on an unknown run or schedule return a `*.not_found` error |
| Pruning is part of the contract | `DurableRunStore::prune` deletes finished runs before a cut-off |

## Agent identity

The `principal` module identifies the agent a run acts as. A `NewRun` carries an optional
`ActingContext`, and the run record keeps it.

| Type | Identifies |
|------|------------|
| `AgentId` | One agent, stable across redeploys: a URI such as `spiffe://example.org/agent/billing` or `urn:adk:agent:billing`; non-empty, at most 512 bytes, no control characters |
| `SubjectRef` | A human, a service, or an agent (`SubjectKind`) by identifier |
| `AgentPrincipal` | An agent with its tenant, class (template), instance (replica), sponsor, and attributes |
| `ActingContext` | An agent acting in one invocation, on behalf of a subject, through a delegation chain (outermost first) |

```rust
use adk_core::principal::{ActingContext, AgentId, AgentPrincipal, SubjectRef};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let agent = AgentPrincipal::new(AgentId::new("urn:adk:agent:billing")?)
        .with_tenant("acme")
        .with_sponsor(SubjectRef::human("alice@example.com"));
    let acting = ActingContext::new(agent, "inv-1")
        .on_behalf_of(SubjectRef::human("bob@example.com"));
    assert_eq!(serde_json::to_value(&acting)?["onBehalfOf"]["kind"], "human");
    Ok(())
}
```

## Leases and fencing

`LeaseStore::acquire` grants a resource to one owner for a time to live. Each grant's `epoch`
is one higher than the resource's previous grant, and epochs never repeat, even after release.

| Operation | Free, expired, or released | Held by the caller | Held by another owner |
|-----------|----------------------------|--------------------|-----------------------|
| `acquire` | Granted at epoch + 1 | Granted again at epoch + 1 | `None` |
| `renew` | Extended while the epoch is current and the lease was not released | Extended | `lease.stale_epoch` |
| `release` | No-op | Released | No-op |

Stale epochs and compare-and-swap conflicts (`lease.stale_epoch`, `run_store.stale_epoch`,
`governance.conflict`) carry `ErrorCategory::Conflict`, which maps to HTTP 409.

A holder that stalls past its expiry while another owner takes the resource holds a lower
epoch, so its next renew fails with `lease.stale_epoch` and any store that checks the epoch
refuses its writes.

## Runs

A worker claims a run, heartbeats while it works, and finishes it. Every worker write —
`heartbeat`, `set_checkpoint`, `wait`, `finish` — carries the epoch returned by `claim` and
fails with `run_store.stale_epoch` once the run is no longer `Running` at that epoch.

| From | Event | To | Attempt | Epoch |
|------|-------|----|---------|-------|
| — | `enqueue` | `Queued` | 0 | 0 |
| `Queued` | `claim`, once `not_before` passed and no earlier run holds its concurrency key | `Running` | + 1 | + 1 |
| `Running` | `claim` after the lease expired (recovery) | `Running` | + 1 | + 1 |
| `Running` | `wait(until)` | `Waiting`, lease released | — | — |
| `Waiting` | `claim` once `until` passed or `wake` was called | `Running` | — | + 1 |
| `Running` | `finish(Completed)` | `Completed` | — | — |
| `Running` | `finish(Failed { retryable: true })` with attempts left and no cancel request | `Queued` | — | — |
| `Running` | `finish(Failed)` otherwise | `Failed` | — | — |
| `Running` | `finish(Cancelled)` | `Cancelled` | — | — |
| `Queued`, `Waiting` | `request_cancel` | `Cancelled` | — | — |
| `Running` | `request_cancel` | `Running` with `cancel_requested`, returned by the next `heartbeat` | — | — |

A recovery claim of a run that already used `max_attempts` attempts fails it instead. A
`wake` that arrives while the run is still `Running` — an approval decided before the worker
parked the run — sets `wake_pending`, and the next `wait` parks the run as already woken.

### Idempotency and double texting

- **Idempotency key**: unique per queue. `enqueue` returns `Enqueued::Existing` with the
  existing run until that run is pruned.
- **Concurrency key**: runs sharing a key in one queue run one at a time, in enqueue order.
  `OnConflict` decides what `enqueue` does when an unfinished run holds the key.

| `OnConflict` | Effect |
|--------------|--------|
| `Reject` | `Enqueued::Rejected { blocking_run_id }`, naming the oldest unfinished run |
| `Enqueue` (default) | Queues the run behind the unfinished ones |
| `Interrupt` | Cancels queued and waiting holders, requests cancellation of running ones, and queues the run behind them |

```rust
use std::time::Duration;

use adk_core::{DurableRunStore, InMemoryRunStore, NewRun, OnConflict, RunOutcome};
use serde_json::json;

#[tokio::main]
async fn main() -> adk_core::Result<()> {
    let runs = InMemoryRunStore::new();
    let run = NewRun::new("default", "support", "user-1", "triage_agent", json!({ "text": "hi" }))
        .with_idempotency_key("webhook:evt_123")
        .with_concurrency_key("thread:42", OnConflict::Interrupt);
    runs.enqueue(run).await?;

    let claimed = runs.claim("default", "worker-1", Duration::from_secs(30)).await?.unwrap();
    let cancel_requested = runs.heartbeat(&claimed.record.run_id, claimed.epoch, Duration::from_secs(30)).await?;
    assert!(!cancel_requested);
    runs.finish(&claimed.record.run_id, claimed.epoch, RunOutcome::Completed(json!("done"))).await?;
    Ok(())
}
```

## Schedules and the firing rule

A `Schedule` pairs a `ScheduleSpec` — `Cron { expr, timezone }`, `At(instant)`, or
`Every(interval)` — with a `NewRun` template and a `MissedFirePolicy`. A scheduler on each
replica runs the same loop:

1. Read `ScheduleStore::due(now, limit)`.
2. For each schedule, call `schedule::plan_fires(&spec, missed, next_fire_at, now, grace)`.
3. Enqueue `schedule.run_for(fire_time)` for every planned fire time. Each run's idempotency
   key is `schedule:{id}:{fire_time RFC 3339}`, so every replica that plans the same fire time
   enqueues the same run once.
4. Call `ScheduleStore::advance(id, revision, plan.next_fire_at)`. The compare-and-swap on the
   revision lets one replica move the schedule; the others get `false`.

A replica that crashes between steps 3 and 4 leaves the schedule due; the next replica plans
the same fire times, and their idempotency keys enqueue nothing new.

| `MissedFirePolicy` | Fires when several occurrences are due |
|--------------------|----------------------------------------|
| `Skip` | The latest due occurrence, only if it is at most `grace` late (`DEFAULT_MISFIRE_GRACE` is 60 seconds) |
| `FireOnce` | The latest due occurrence, however late |
| `CatchUp(n)` | The latest `n` due occurrences, oldest first |

Cron expressions take five fields (`min hour dom month dow`) or six or seven (`sec min hour
dom month dow [year]`) and are evaluated in an IANA time zone. A local time skipped by a
spring-forward change does not fire; a local time repeated by a fall-back change fires once.
Evaluating cron specs needs the `cron` feature:

```toml
[dependencies]
adk-core = { version = "3.0.0", features = ["cron"] }
```

`ScheduleStore::upsert` assigns revisions: 1 on insert, the previous revision plus one on a
change, and no change for an identical definition, so replicas registering the same schedule
at startup do not disturb an advance in flight.

## Governance state

`GovernanceStateStore` records whether a `GovernanceScope` — `Org`, `Tenant(name)`, or
`Agent(AgentId)` — is frozen, who froze it, and why. `set` is a compare-and-swap on the
scope's epoch (0 when it has no state); a moved epoch fails with `governance.conflict`, whose
`current_epoch` metadata holds the epoch to re-read from.

## Worker

`adk_server::background::DurableWorker` (feature `background`) claims runs from a
`DurableRunStore` under fenced leases and executes them: agents through their `Runner`,
workflows through a `WorkflowExecutor`. Run one per replica; every replica claiming from the
same store and queue shares the work.

| `DurableWorkerConfig` field | Default | Meaning |
|-----------------------------|---------|---------|
| `worker_id` | `worker-{uuid}` | Owner name written into every lease |
| `queue` | `default` | Queue claimed from |
| `lease_ttl` | 30 s | How long a claim or heartbeat holds the run; heartbeats run every `lease_ttl / 3` |
| `poll_interval` | 500 ms | Wait when nothing is claimable |
| `max_concurrent_runs` | 4 | Attempts executed at once |
| `run_timeout` | `None` | Fails an attempt that runs longer (not retried) |
| `retain_finished` | 24 h | Finished runs older than this are pruned by the claim loop |
| `approval_timeout` | `None` | How long a run parked on a held tool call waits for a decision; `None` waits until woken |

One attempt of a claimed run:

1. `claim` returns the run with a fencing epoch. A worker whose governance control refuses work
   (frozen, or a stale fail-closed mirror) claims nothing.
2. The worker records the checkpoint with `set_checkpoint` before any progress is made.
3. A heartbeat every `lease_ttl / 3` extends the lease. A heartbeat that reports a cancellation
   request stops the run cooperatively; one refused with `run_store.stale_epoch` drops the
   execution at once, and the worker writes nothing more.
4. `finish` writes the outcome, fenced by the epoch: `Completed`, `Failed { retryable }`
   (retryable failures requeue while attempts remain), or `Cancelled` when cancellation was
   requested.

**Lease fence.** For agent runs the worker installs invocation hooks around every model call,
model response, tool call, and tool result. Each checks that the lease is still provably held —
the last successful heartbeat was sent less than `lease_ttl` ago. When it is not, the hook never
returns and the worker drops the attempt, so a stalled worker makes no further model call, tool
call, session write, or store write.

| `WorkerHandle` method | Effect |
|-----------------------|--------|
| `shutdown().await` | Stops claiming and waits for in-flight attempts to write their outcome |
| `wake()` | Tries a claim now instead of after `poll_interval` |
| `heartbeat_now()` | Makes every in-flight attempt heartbeat now, so a cancellation requested in this process is seen at once |
| `abort()` | Drops the loop and every in-flight attempt without writing, as a crash would; their leases expire and another worker recovers them |

```rust,ignore
use std::sync::Arc;

use adk_server::background::{DurableWorker, DurableWorkerConfig, WorkflowRegistry};

let worker = DurableWorker::new(run_store)
    .with_agent("support", Arc::new(runner))
    .with_workflows(Arc::new(WorkflowRegistry::new()))
    .with_governance(governance.clone())
    .with_config(DurableWorkerConfig::default().with_queue("default"));
let handle = worker.spawn();
// ...
handle.shutdown().await;
```

## Parking on held tool calls

An agent run that ends holding a tool call for approval (no decision available) parks
instead of finishing:

1. The worker records `RunCheckpointRef::AwaitingApproval { session_id, invocation_id,
   fingerprints, expires_at }`. `fingerprints` are the held calls' decision keys
   (`ToolConfirmationRequest::fingerprint`); `expires_at` is `now + approval_timeout` when one
   is configured.
2. It calls `DurableRunStore::wait(run_id, epoch, expires_at)`; the run is `Waiting` and holds
   no lease.
3. The approval service records the decision and wakes the run with
   `BackgroundState::wake_run(run_id)`, `BackgroundState::wake_session_runs(app, user,
   session)`, or `POST /runs/{run_id}/wake`. A wake that lands before the run parks is kept
   (`wake_pending`).
4. The next claim continues at the same attempt and resumes the same session with an empty user
   turn; the agent dispatches the held call under its original call id through the governed
   path, so the approved call executes once and a denied one is answered as denied.

A run claimed again past `expires_at` with the same calls still held fails with
`approval expired`.

## Crash resume

A run recovered after its lease expired resumes rather than restarting. No tool executes again
during recovery.

| Target | Checkpoint | What the recovering worker does |
|--------|------------|---------------------------------|
| Agent | `Session { session_id, invocation_id }`, recorded before the event stream is polled, so the user turn never reaches the session without a checkpoint naming it | 1. When the session's last event is the checkpoint invocation's final answer, completes the run with it — no model call. 2. Otherwise answers every unanswered function call from the action ledger (`Runner::recover_unanswered_calls`). 3. Continues the same session with an empty user turn when the original user turn was persisted, so nothing is sent twice; otherwise starts from the run's input |
| Workflow | `Graph { thread_id, checkpoint_id }`; `thread_id` is the run id | Calls the workflow again with `WorkflowRunContext::resumed` set and the same `thread_id`; a graph workflow invoked with `ExecutionConfig::new(&run.thread_id)` resumes from its checkpointer |

`Runner::recover_unanswered_calls` keys each call by its original idempotency key
(`app/user/session/invocation/call`):

| Ledger record | Response appended |
|---------------|-------------------|
| Completed with a value | `already_succeeded` with the result digest |
| Completed with an error | `already_failed` with the error |
| Begun with no outcome, or none | `outcome_unknown` |

A workflow registered with `WorkflowRegistry::register_durable` (or an executor overriding
`WorkflowExecutor::execute_durable`) receives the `WorkflowRunContext`, and may report
checkpoints with `record_checkpoint`, which is fenced by the claim's epoch. A workflow
registered with `register` restarts from its input.

```rust,ignore
use adk_graph::node::ExecutionConfig;
use adk_graph::state::State;
use adk_server::background::WorkflowRegistry;

let registry = WorkflowRegistry::new().register_durable("settle", move |input, run| {
    let graph = graph.clone();
    async move {
        let state: State = input.into_iter().collect();
        let out = graph
            .invoke(state, ExecutionConfig::new(&run.thread_id))
            .await
            .map_err(|error| error.to_string())?;
        Ok(serde_json::json!({ "receipt": out.get("receipt") }))
    }
});
```

**Across processes.** `adk-server/tests/durable_multiprocess.rs` runs the gate with separate
OS processes on SQLite files (PR tier) and PostgreSQL (`#[ignore]`, nightly `durable-stores`):
the process running the charge is killed with SIGKILL mid-call, and a second process completes
the run with one charge, answered `outcome_unknown` from the durable action ledger.

**Known limits.**

- Session events a stalled worker appended before its fence tripped remain in the session.
- A model that issues a call again after an `outcome_unknown` response issues a new call with a
  new idempotency key; the ledger does not deduplicate it. Payment tools keep their own
  business idempotency key for that case.

## Scheduler

`adk_server::background::DurableScheduler` enqueues each schedule's fire times exactly once,
from any number of replicas. Each `tick`:

1. Reads `ScheduleStore::due`.
2. Plans the fire times with `plan_fires` and the schedule's `MissedFirePolicy`.
3. Enqueues `Schedule::run_for(fire_time)`, keyed `schedule:{id}:{fire_time}`, so replicas that
   plan the same fire time create one run.
4. Advances the schedule with `ScheduleStore::advance`, a compare-and-swap on its revision; a
   replica that loses the race drops its plan.

While the governance control refuses work, nothing fires and nothing advances. Once the freeze
lifts, the missed-fire policy decides which fire times that fell due during it run.
`start_cron_scheduler` runs the tick loop every second; start it on every replica.

```rust,ignore
use std::sync::Arc;

use adk_server::background::DurableScheduler;

let handle = DurableScheduler::new(schedule_store, run_store)
    .with_governance(governance.clone())
    .spawn();
```

## Replicated kill switch

`GovernanceControl::mirrored(store, config)` builds a kill switch that mirrors a
`GovernanceStateStore` shared by every replica. `GovernanceControl::new` stays process-local.

| `GovernanceMirrorConfig` field | Default | Meaning |
|--------------------------------|---------|---------|
| `poll_interval` | 2 s | How often `spawn_mirror` reads the store |
| `max_staleness` | 30 s | How long the last successful read is trusted |
| `fail_open` | `false` | Keep running when the store cannot be read past `max_staleness` |
| `scopes` | `[GovernanceScope::Org]` | Mirrored scopes; frozen when any is; the first is the one replicated freezes write |
| `watch` | `None` | A `GovernanceWatch` change notification that triggers a read before the next poll |

| Method | Behaviour |
|--------|-----------|
| `spawn_mirror` | Reads the store at once, then every `poll_interval` and on each `GovernanceWatch` notification |
| `refresh` | One read of the mirrored scopes |
| `freeze_replicated` | Compare-and-swap write of the first scope, retried on `governance.conflict`, then applied locally; when the store cannot be written the replica freezes locally and the error is returned |
| `unfreeze_replicated` | Compare-and-swap write, then clears the local freeze too |
| `is_stale`, `staleness` | Time since the last successful read |

A mirrored control is stale until its first read. Past `max_staleness` without a successful
read, `check` fails closed with `governance.stale` (`ErrorCategory::Unavailable`, retryable;
`GOVERNANCE_STALE_CODE`): new runs are refused and running ones stop at their next model or tool
check, as under a freeze (`governance.frozen`, `GOVERNANCE_FROZEN_CODE`). A local `freeze` on a
mirrored control holds until a local `unfreeze`; the store never lifts it.

The server's `POST /api/admin/freeze` and `POST /api/admin/unfreeze` call `freeze_replicated`
and `unfreeze_replicated`, so a mirrored `ServerConfig::governance` reaches every replica. A
freeze also requests cancellation of every running run in the background run stores mounted on
the builder. Responses report `frozen`, `reason`, `mirrored`, `stale`, and, for a freeze,
`cancelledRuns`; a store write failure returns `503` with `error`.

```rust,ignore
use std::sync::Arc;
use std::time::Duration;

use adk_core::GovernanceControl;
use adk_core::governance::GovernanceMirrorConfig;

let control = GovernanceControl::mirrored(
    governance_store,
    GovernanceMirrorConfig::default().with_poll_interval(Duration::from_secs(1)),
);
let _mirror = control.spawn_mirror();
let config = ServerConfig::new(loader, sessions).with_governance(control);
```

## Cancel across replicas

| Entry point | Effect |
|-------------|--------|
| `DELETE /runs/{id}` | `request_cancel`: a queued or waiting run is cancelled at once; a running run is flagged |
| `Runner::interrupt(session_id)` | Cancels local runs, and, when the runner has a durable run store (`RunnerConfig::durable_runs`, builder `durable_run_store`), requests cancellation of the session's unfinished durable runs on a spawned task |
| `Runner::cancel_durable_runs(user_id, session_id).await` | The same store request, awaited; returns how many runs were asked to stop |

The worker that owns a flagged run sees the request on its next heartbeat, within
`lease_ttl / 3`, interrupts the agent or fires the workflow's cancellation token, and finishes
the run `Cancelled`.

## Backend matrix

| Backend | `LeaseStore` | `DurableRunStore` | `ScheduleStore` | `GovernanceStateStore` |
|---------|--------------|-------------------|-----------------|------------------------|
| In memory (`adk-core`) | `InMemoryLeaseStore` | `InMemoryRunStore` | `InMemoryScheduleStore` | `InMemoryGovernanceStateStore` |
| SQLite (`adk-session`, `sqlite`) | `SqliteLeaseStore` | `SqliteRunStore` | `SqliteScheduleStore` | `SqliteGovernanceStateStore` |
| PostgreSQL (`adk-session`, `postgres`) | `PostgresLeaseStore` | `PostgresRunStore` | `PostgresScheduleStore` | `PostgresGovernanceStateStore` |
| Redis (`adk-session`, `redis`) | `RedisLeaseStore` | `RedisRunStore` | `RedisScheduleStore` | `RedisGovernanceStateStore` |
| MongoDB (`adk-session`, `mongodb`) | `MongoLeaseStore` | `MongoRunStore` | `MongoScheduleStore` | `MongoGovernanceStateStore` |
| Neo4j (`adk-session`, `neo4j`) | `Neo4jLeaseStore` | `Neo4jRunStore` | `Neo4jScheduleStore` | `Neo4jGovernanceStateStore` |
| Firestore (`adk-session`, `firestore`) | `FirestoreLeaseStore` | `FirestoreRunStore` | `FirestoreScheduleStore` | `FirestoreGovernanceStateStore` |

The Redis, MongoDB, Neo4j, and Firestore types live in `adk-session` behind the backend's
existing feature. Each has `new(...)`, a constructor over the handle a session service holds (`from_client`,
`from_database`, `from_graph`, `from_db`), and `migrate()`; the lease, run, and governance
stores add `with_clock`.

```rust
use std::sync::Arc;
use std::time::Duration;

use adk_core::{DurableRunStore, NewRun, RunOutcome};
use adk_session::RedisRunStore;
use serde_json::json;

#[tokio::main]
async fn main() -> adk_core::Result<()> {
    let runs: Arc<dyn DurableRunStore> = {
        let store = RedisRunStore::new("redis://localhost:6379").await?;
        store.migrate().await?;
        Arc::new(store)
    };
    runs.enqueue(NewRun::new("default", "billing", "user-1", "invoice_agent", json!({}))).await?;
    if let Some(claimed) = runs.claim("default", "worker-1", Duration::from_secs(30)).await? {
        let run_id = &claimed.record.run_id;
        runs.finish(run_id, claimed.epoch, RunOutcome::Completed(json!("done"))).await?;
    }
    Ok(())
}
```

The atomic primitive each guarantee needs:

| Guarantee | SQL | Redis | MongoDB | Neo4j, Firestore |
|-----------|-----|-------|---------|------------------|
| Single lease winner | Conditional upsert on the resource row | One Lua script | `findOneAndUpdate` with `upsert` | Transaction over the resource record |
| Idempotent enqueue | Unique index on `(queue, idempotency_key)` | Lua create-if-absent | Unique partial index, inside a transaction on a replica set | Transaction over the key record |
| Single claim winner | `SELECT ... FOR UPDATE SKIP LOCKED` (PostgreSQL), `BEGIN IMMEDIATE` (SQLite) | One Lua script | Replace filtered on the run's `revision` | Transaction that re-reads the run and writes the new epoch |
| Fenced worker writes | `UPDATE ... WHERE lease_epoch = $epoch` | Lua epoch compare | Replace filtered on the `revision` after the epoch check | Transaction that checks the epoch |
| Schedule advance, governance set | `UPDATE ... WHERE revision = $expected` | Lua compare | Filtered `updateOne` or replace | Transaction that checks the revision |

### Backend notes

| Backend | Atomicity | Storage | Notes |
|---------|-----------|---------|-------|
| Redis | Every conditional change is one Lua script | One hash per lease and run; sorted sets of ready runs (by enqueue order), timers (delays, waits, lease expiry), key holders, and due schedules | No key expiry: deadlines are timestamps compared in the scripts, so the durable truth never depends on eviction. Each store keeps its keys under one hash tag, so it runs on Redis Cluster on one slot per store |
| MongoDB | On a replica set (one node is enough) or sharded cluster, every enqueue is one multi-document transaction; everything else is a single-document write: filtered `findOneAndUpdate`, revision-filtered replace, unique indexes | `adk_leases`, `adk_runs` (record as JSON plus queried fields), `adk_run_keys`, `adk_schedules`, `adk_governance` | A concurrency key's `adk_run_keys` document lists its unfinished holders in order; only the first is claimable. `MongoRunStore::supports_transactions` detects the server type from `hello` (`with_transactions` overrides it). A standalone `mongod` uses a weaker fallback with two windows: a `Reject` that loses admission deletes the run it inserted, which a concurrent enqueue with the same idempotency key can briefly see as `Existing`; and a keyed run whose enqueue stopped before admission is admitted by a claim a minute later |
| Neo4j | Transactions that write-lock a lock node before reading | `AdkLease`, `AdkRun` (record as JSON), `AdkRunQueue`, `AdkSchedule`, `AdkGovernanceState` nodes | Enqueues and claims serialize per queue on the `AdkRunQueue` node; worker writes lock the run node. Transient errors such as deadlocks are retried up to 30 times |
| Firestore | Read-write transactions that read documents by ID only | `adk_leases`, `adk_runs`, `adk_run_queues`, `adk_run_keys`, `adk_run_key_holders`, `adk_schedules`, `adk_governance` | Enqueues serialize per queue on the `adk_run_queues` document. A claim queries candidates outside any transaction, then re-reads and claims each in its own transaction. A concurrency key's holders live in one document. Every query ranges over one field, so no composite index is needed. Claims take the run that became claimable first, then the oldest enqueued; for undelayed queued runs that is enqueue order |

### SQL stores

The SQLite and PostgreSQL stores share one schema shape and, where the dialects agree, one
statement text. Each has `new(url)`, `from_pool` over the pool a session service uses, and
`migrate()`, which applies versioned steps and, on PostgreSQL, holds an advisory lock so
replicas can migrate at startup. Timestamps are stored as microseconds.

| Store | Table | Primitive |
|-------|-------|-----------|
| `*LeaseStore` | `adk_leases` | `acquire` is one `INSERT ... ON CONFLICT (resource) DO UPDATE ... WHERE released OR expires_at_us <= now OR owner = $owner RETURNING epoch`; `renew` and `release` are `UPDATE ... WHERE epoch = $epoch` |
| `*RunStore` | `adk_runs` | Unique index on `(queue, idempotency_key)`; PostgreSQL claims with `SELECT ... FOR UPDATE OF r SKIP LOCKED` and serializes enqueues per concurrency key with `pg_advisory_xact_lock`, SQLite runs both under `BEGIN IMMEDIATE`; fenced writes are `UPDATE ... WHERE run_id = $1 AND lease_epoch = $2 AND state = 'running'` |
| `*ScheduleStore` | `adk_schedules` | `advance` is `UPDATE ... WHERE id = $1 AND revision = $2`; `due` reads the `(paused, next_fire_at_us)` index |
| `*GovernanceStateStore` | `adk_governance_state` | `INSERT ... ON CONFLICT DO NOTHING` from epoch 0, `UPDATE ... WHERE epoch = $expected` otherwise |

`prune` deletes finished runs in batches of 500, so one call never holds a long lock.

```rust
use std::time::Duration;

use adk_core::{AdkError, DurableRunStore, LeaseStore, NewRun};
use adk_session::{PostgresLeaseStore, PostgresRunStore, PostgresSessionService};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;

#[tokio::main]
async fn main() -> adk_core::Result<()> {
    // One pool serves the session service and every durable store.
    let pool = PgPoolOptions::new()
        .max_connections(20)
        .connect("postgres://localhost/app")
        .await
        .map_err(|e| AdkError::session(e.to_string()))?;
    let sessions = PostgresSessionService::from_pool(pool.clone());
    sessions.migrate().await?;
    let leases = PostgresLeaseStore::from_pool(pool.clone());
    leases.migrate().await?;
    let runs = PostgresRunStore::from_pool(pool);
    runs.migrate().await?;

    if let Some(leader) = leases.acquire("scheduler", "replica-1", Duration::from_secs(30)).await? {
        runs.enqueue(NewRun::new("default", "my-app", "user-1", "assistant", json!({}))).await?;
        leases.release(&leader).await?;
    }
    Ok(())
}
```

## Conformance suites

The suites live in `adk-core` behind the `conformance` feature, alongside the
[durable store suites](../security/durable-stores.md#conformance-suite). Time-dependent
harnesses receive the clock to read.

```rust
use std::sync::Arc;

use adk_core::conformance::RunStoreHarness;
use adk_core::{Clock, DurableRunStore, InMemoryRunStore, async_trait};

struct InMemory;

#[async_trait]
impl RunStoreHarness for InMemory {
    async fn open(&self, clock: Arc<dyn Clock>) -> Arc<dyn DurableRunStore> {
        Arc::new(InMemoryRunStore::new().with_clock(clock))
    }

    fn is_durable(&self) -> bool {
        false
    }
}

adk_core::run_store_conformance_tests!(harness = InMemory);
```

| Suite | Cases |
|-------|-------|
| `lease_store_conformance_tests!` | acquire, renew, and release with epochs; 32 concurrent acquirers across two handles with one winner; expiry takeover at the next epoch; stale renew and release refused; restart survival |
| `run_store_conformance_tests!` | round trip; 32 concurrent idempotent enqueues with one run; `Reject`, `Enqueue`, and `Interrupt`; 32 concurrent claimers with one winner per run; `not_before`; expired-lease recovery and exhausted attempts; stale-epoch `heartbeat`, `set_checkpoint`, `wait`, and `finish` refused; retryable failures until `max_attempts`; wait and wake; cancellation through heartbeat; prune; restart survival |
| `schedule_store_conformance_tests!` | idempotent upsert; due ordering and limit; 32 concurrent advances with one winner; paused schedules never due; restart survival |
| `governance_state_store_conformance_tests!` | set, get, and list with the epoch CAS; 32 concurrent sets with one winner; independent scopes; restart survival |

Identifiers in every case contain `:`, `/`, and non-ASCII characters, so a backend that builds
keys from them is checked for escaping. Restart cases and two-handle concurrency are skipped
for a harness whose `is_durable` returns `false`.

| Backend | Command |
|---------|---------|
| In memory | `cargo nextest run -p adk-core --test store_conformance` |
| SQLite | `cargo nextest run -p adk-session --features sqlite --test store_conformance_sqlite` |
| PostgreSQL | `DATABASE_URL=postgres://... cargo nextest run -p adk-session --features postgres --run-ignored only --test store_conformance_postgres --test run_store_multiprocess_postgres` |
| Redis | `REDIS_URL=redis://localhost:6379 cargo nextest run -p adk-session --features redis --run-ignored only --test store_conformance_redis` |
| MongoDB | `MONGODB_URL=mongodb://localhost:27017 cargo nextest run -p adk-session --features mongodb --run-ignored only --test store_conformance_mongodb` |
| Neo4j | `NEO4J_URI=bolt://localhost:7687 NEO4J_USER=neo4j NEO4J_PASSWORD=<password> cargo nextest run -p adk-session --features neo4j --run-ignored only --test store_conformance_neo4j` |
| Firestore | `FIRESTORE_EMULATOR_HOST=localhost:8080 cargo nextest run -p adk-session --features firestore --run-ignored only --test store_conformance_firestore` |

`run_store_multiprocess_postgres` starts three OS processes that claim from one queue of
200 runs at once and checks that each run is claimed and finished exactly once. The SQLite
suites run on every pull request; the PostgreSQL suites run in the nightly `durable-stores`
job.

Each backend file also runs a contention case beyond the suites: 24 claimers, half on each of
two handles, drain one queue of 48 runs (16 claimers and 16 runs on the Firestore emulator),
and every run is claimed exactly once. The nightly
`durable-stores` job runs all of them against live servers and the Firestore emulator.

## Related

- [Durable Stores](../security/durable-stores.md)
- [Tool Authorization](../security/tool-authorization.md)
- [Runner](runner.md)
- [Background runs and cron](../agents/functional-api.md)
