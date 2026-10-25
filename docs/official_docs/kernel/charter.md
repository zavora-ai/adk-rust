# Charter, KPIs, Signals and Earned Autonomy

The `charter` feature of `adk-kernel` keeps a website's charter and enforces it (spec 3.5
§7.10, §7.11): every committed charter version, the KPI series the charter measures, each
job's autonomy level, and the typed signals the public partition sends the back office. The
charter format is AWP 2.0's, `awp_types::Charter`; the kernel stores it, measures against it
and holds the site to it.

```toml
[dependencies]
adk-kernel = { version = "3.0.0", features = ["charter"] }
```

| Part | Type | Invariant |
|---|---|---|
| Charter store | `CharterStore`, `CharterVersion`, `CommitDecision` | A-I14, A-I23 |
| KPI store and tracker | `KpiStore`, `KpiSource`, `KpiTracker`, `KpiTrackerExecutor` | A-I14, A-I15 |
| Job pauses | `JobControl`, `JobPauses` | A-I15 |
| Autonomy ladder | `AutonomyStore`, `AutonomyLadder`, `OwnerDecision` | A-I13 |
| Signals | `SignalStore`, `SignalSink`, `SignalSourceTool` | A-I3 |
| Site process kinds | `SiteProcessKind` | — |

Everything lives under `adk_kernel::charter`.

## The charter store

A charter version takes effect only through `CharterStore::commit(draft, decision)`:

| Check | Refusal |
|---|---|
| The draft passes `Charter::validate()`: no KPI target without a baseline (A-I14), at least one owner, no agent holding a role | `KernelError::Config` |
| No role holder is an agent identity (`urn:adk:agent:…` or `spiffe://…`) | `KernelError::Config` |
| The decision satisfies the `charter_change` approval chain of the charter in force — the draft's own chain for a site's first charter, so a draft cannot loosen the chain that approves it — by distinct people, each acting in a role they hold, none of them an agent (A-I23) | `KernelError::Unauthorized` |
| Another commit took the version first | a `Conflict` error |

A charter that names no `charter_change` chain needs one owner. A committed version is the
site's next number and carries `OwnerSignature { decision, principal, note, signed_at }`:
the decision's id, the first owner who approved, the signed note that seals it in the audit
log, and the time. The store is append-only: `append` takes only the version one past the
latest, so concurrent commits leave a gapless history.

`commit_audited(store, draft, decision, trail)` commits and records the satisfied or refused
chain and the commit in the audit log.

`check_chain(governance, action, approvals, proposer)` is the same check for any action
class — a publication's editor and publisher, for example — and also refuses the proposer
as an approver.

```rust
use std::sync::Arc;

use adk_kernel::charter::{CharterStore, InMemoryCharterStore};
use adk_kernel::conformance::charter::fixtures::{charter, decision, with_chain};
use awp_types::{ActionClass, Role};

#[tokio::main]
async fn main() -> adk_kernel::Result<()> {
    let store = Arc::new(InMemoryCharterStore::new());
    let site = uuid::Uuid::new_v4();
    let now = chrono::Utc::now();
    // A charter change needs both owners.
    let draft = with_chain(charter(site, &["amina", "baraka"]), ActionClass::CharterChange, &[Role::Owner, Role::Owner]);
    let alone = store.commit(draft.clone(), decision("d-1", now, &[(Role::Owner, "amina")])).await;
    assert!(alone.is_err());
    let committed = store
        .commit(draft, decision("d-2", now, &[(Role::Owner, "amina"), (Role::Owner, "baraka")]))
        .await?;
    assert_eq!(committed.version(), 1);
    Ok(())
}
```

The fixtures come from the `conformance` feature:
`adk-kernel = { version = "3.0.0", features = ["charter", "conformance"] }`.

## The KPI tracker

`KpiTracker::tick` reads every KPI of the charter in force whose cadence is due from the
`KpiSource` its `source` names (`beacon`, `host_logs`, `analytics:<driver>`, `commerce`,
`spend`, `email`), records the measurement in the `KpiStore`, and checks the guardrails.

| Rule | What the tracker does |
|---|---|
| Measure-only (A-I14) | A KPI with no baseline is measured for 14 days (`with_measure_only`) from the first tick that saw it and reports no target; then the tracker locks a baseline, the mean of the period's samples, and emits `CharterEvent::BaselineLocked` |
| Targets (A-I14) | A charter target counts only beside a charter baseline; the owner records the baseline and sets the target in the next charter version |
| Guardrails (A-I15) | When a guardrail's metric — the mean over its window of the KPIs that measure it — breaches its threshold, the tracker first pauses each of the guardrail's jobs through its `JobControl` (`PauseJob`), then emits `GuardrailBreached`, then demotes each job one level; a breach acts once until the metric recovers, and overrides any growth target |
| Restarts | `restore_pauses` pauses again the jobs of every guardrail the store still records as breached |

`KpiStore` keeps one sample per KPI and instant and derives daily and ISO-weekly
aggregates: one bucket per period, empty periods included. `KpiTracker::review(week)` gives
each KPI's week against the week before, its baseline and its target for the weekly review.

The tracker runs as the kernel's own maintenance process: register `KpiTrackerExecutor`
with `KernelBuilder::workflows` and `KpiTracker::trigger(app, "0 0 3 * * *")` with
`KernelBuilder::trigger`, beside a `CronWakeSource` for the expression. Each run executes
target `kernel.kpi`, makes no model call, and returns the `TrackerReport`.

`JobPauses` implements `JobControl` and `ToolPolicy`: added to the job's harness policy, it
denies every tool call of a paused job's agents until a person resumes the job with an
`OwnerDecision`.

## The autonomy ladder

| Rung | Level | Mechanism (compiled by the harness) |
|---|---|---|
| 0 | `Observe` | Tools filtered to read-only |
| 1 | `Propose` | Plan mode: writes become drafts and proposals |
| 2 | `ActWithApproval` | Every non-read call held for approval |
| 3 | `ActReversibleOnly` | Auto for actions declared reversible: rollback, read-only, drafts |
| 4 | `ActWithinBudget` | Auto for the job's tool classes within its rules and budget |

`AutonomyLadder` keeps each job's level and history in an `AutonomyStore`:

| Change | Who | Rule |
|---|---|---|
| `register`, `sync(charter)` | the site | A job enters at its charter level; a charter that sets a lower level lowers it; a higher level changes nothing |
| `promote`, `promote_to` | an owner | Takes an `OwnerDecision` by a person who holds the owner role in the charter in force; agents, services and anyone else are refused (A-I13). The change carries the decision, from which the harness creates the standing rule |
| `lower` | the site | One rung down, at any time |
| `guardrail_breach` | the tracker | One rung down |
| `record_decision` | the owner's inbox | The second owner rejection in a row since the job's last change lowers it one rung; decisions by anyone but a person are refused |
| `incident(area)` | the site | Every job of the area one rung down |

`eligibility` and `offers` compute promotions from recorded decisions: 20 unchanged
approvals in a row within 30 days since the job's last change (`PromotionPolicy`). Every
change is audited as `website.job.promoted` or `website.job.demoted` and emitted as
`CharterEvent::AutonomyChanged`.

## Signals

`SignalSink::emit(signal)` files an `awp_types::Signal` in a back-office partition's agenda
(spec 3.5 §7.10):

| Step | What happens |
|---|---|
| Rate limit | The signal's `emitted_by` may send 60 signals a minute (`SignalRateLimit`); past that, nothing is filed and the outcome is `RateLimited` |
| Repeat key | The partition, the kind, and a digest of the fields that name the topic — a content gap's topic compared without case or spacing, a lead, a thread, a requester and capability |
| Filing | The first signal of a key is an agenda item with `AgendaOrigin::Signal { kind, count: 1 }`; a repeat while the item is open raises `count` instead of adding an item |
| Content | The item's rationale holds the typed fields only, marked untrusted; the kernel keeps no raw text |
| Wake | A `signal:` wake event wakes processes sleeping on `Wake::Signal { kind, partition }` |
| Record | `kernel.signal` with the kind, topic digest, item, count, source and outcome, and no field text |

Only a person turns a signal item into work. A signal is not a crossing: it reads nothing
back from the public partition. A back-office request for the raw text behind a signal is a
crossing: `SignalSourceTool` registers `signal_source_text` with the back-office
`PartitionPolicy` as a crossing into the public partition's sessions, and the policy denies
it to every agent, the coordinator included, since the public partition crosses nowhere.

```rust
use std::sync::Arc;

use adk_kernel::InMemoryAgendaStore;
use adk_kernel::charter::{InMemorySignalStore, SignalOutcome, SignalSink};
use adk_kernel::conformance::charter::fixtures::content_gap;

#[tokio::main]
async fn main() -> adk_kernel::Result<()> {
    let sink = SignalSink::new(Arc::new(InMemorySignalStore::new()), Arc::new(InMemoryAgendaStore::new()), "back-office");
    let first = sink.emit(content_gap("Do you open on Sundays?", 3)).await?;
    assert!(matches!(first, SignalOutcome::Filed { count: 1, .. }));
    let again = sink.emit(content_gap("do you open on  sundays?", 1)).await?;
    assert!(matches!(again, SignalOutcome::Counted { count: 2, .. }));
    Ok(())
}
```

On a running kernel, `SignalSink::for_kernel(&kernel, store, partition)` uses the kernel's
agenda store, wake router, event bus and clock.

## Site process kinds

`SiteProcessKind` names the processes a website's team runs; the product does their work.
A site process is a `ProcessKind::Triggered` process whose source is the kind's name,
labelled `site.process`.

| Kind | Job | Default wake | Queue |
|---|---|---|---|
| `site.probe` | Drift probes of the served release | every 5 minutes | `maintenance` |
| `site.watch` | Advisories, certificates, domains, status | daily at 06:00 UTC | `maintenance` |
| `site.content` | Freshness and content gaps | a `content_gap` signal | `background` |
| `site.reviews` | Review replies | a `review_seen` signal | `background` |
| `site.outreach` | Nurture sequences and handoffs | daily at 09:00 UTC | `background` |
| `site.support` | Support escalations | a `support_escalation` signal | `background` |
| `site.digest` | The weekly review | Mondays at 07:00 UTC | `background` |

## Events and urgency

`CharterEventSink` receives every `CharterEvent` for the product's alarms and weekly review
(spec 3.5 §7.21):

| Event | Urgency |
|---|---|
| `GuardrailBreached`, `JobPaused` | P2: within the hour on the owner's channel, outside quiet hours |
| `Committed`, `BaselineLocked`, `AutonomyChanged`, `PromotionOffered` | P3: the weekly review |

## Backends

| Store | In-memory | SQLite (`sqlite`) | PostgreSQL (`postgres`) | Redis (`redis`) |
|---|---|---|---|---|
| `CharterStore` | `InMemoryCharterStore` | `SqliteCharterStore` | `PostgresCharterStore` | `RedisCharterStore` |
| `KpiStore` | `InMemoryKpiStore` | `SqliteKpiStore` | `PostgresKpiStore` | `RedisKpiStore` |
| `AutonomyStore` | `InMemoryAutonomyStore` | `SqliteAutonomyStore` | `PostgresAutonomyStore` | `RedisAutonomyStore` |
| `SignalStore` | `InMemorySignalStore` | `SqliteSignalStore` | `PostgresSignalStore` | `RedisSignalStore` |

The SQL stores share one migration registry, `_adk_kernel_charter_migrations`, and sit on
the pool of `harness.db` beside the other kernel stores. Every compare-and-set — the next
charter version, a job's next revision, a signal's count — is one statement, a transaction
that writes first, or one Lua script. Each store passes its conformance suite
(`charter_store_conformance_tests!`, `kpi_store_conformance_tests!`,
`autonomy_store_conformance_tests!`, `signal_store_conformance_tests!`, features `charter`
and `conformance`).

## Audit records

| Action | Fields |
|---|---|
| `kernel.signal` | `partition`, `kind`, `topic`, `item`, `count`, `source`, `outcome` (`filed`, `counted`, `rate_limited`) |
| `website.approval_chain.satisfied`, `website.charter.committed` | `site`, `action`, `decision`, `approvers`, `charterVersion`, `signedNote` |
| `website.approval_chain.refused` | `site`, `action`, `decision`, `approvers`, `reason` |
| `website.job.promoted`, `website.job.demoted` | `site`, `job`, `from`, `to`, `reason`, `decision` |
| `website.job.paused` | `site`, `job`, `guardrail`, `metric`, `value`, `threshold`, `charterVersion` |
