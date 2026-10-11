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

The run store is named `DurableRunStore` so it does not clash with
`adk_server::background::RunStore`, the server's process-local record of background runs.

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

## Backend matrix

| Backend | `LeaseStore` | `DurableRunStore` | `ScheduleStore` | `GovernanceStateStore` |
|---------|--------------|-------------------|-----------------|------------------------|
| In memory (`adk-core`) | `InMemoryLeaseStore` | `InMemoryRunStore` | `InMemoryScheduleStore` | `InMemoryGovernanceStateStore` |
| SQLite | Planned | Planned | Planned | Planned |
| PostgreSQL | Planned | Planned | Planned | Planned |
| Redis | Planned | Planned | Planned | Planned |
| MongoDB | Planned | Planned | Planned | Planned |
| Neo4j | Planned | Planned | Planned | Planned |
| Firestore | Planned | Planned | Planned | Planned |

The atomic primitive each guarantee needs:

| Guarantee | SQL | Redis | MongoDB | Neo4j, Firestore |
|-----------|-----|-------|---------|------------------|
| Single lease winner | Conditional upsert on the resource row | One Lua script | `findOneAndUpdate` with `upsert` | Transaction over the resource record |
| Idempotent enqueue | Unique index on `(queue, idempotency_key)` | Lua create-if-absent | Unique index | Transaction over the key record |
| Single claim winner | `SELECT ... FOR UPDATE SKIP LOCKED` (PostgreSQL), `BEGIN IMMEDIATE` (SQLite) | One Lua script | `findOneAndUpdate` | Transaction that writes the new epoch |
| Fenced worker writes | `UPDATE ... WHERE lease_epoch = $epoch` | Lua epoch compare | `updateOne` filtered on the epoch | Transaction that checks the epoch |
| Schedule advance, governance set | `UPDATE ... WHERE revision = $expected` | Lua compare | Filtered `updateOne` | Transaction that checks the revision |

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

## Related

- [Durable Stores](../security/durable-stores.md)
- [Tool Authorization](../security/tool-authorization.md)
- [Runner](runner.md)
