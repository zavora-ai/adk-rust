# Proactivity: the agenda

The agenda is the kernel's todo list: work that outlives a session and may become a process
(spec 3.2 §7.5). An agent proposes follow-ups for itself with `propose_followup`; a person, or
an Auto partition's policy, approves them; the kernel runs each one as a process outside
quiet hours and within the day's budget. Proposing is free; running is governed.

## Agenda items

| Field | Meaning |
|---|---|
| `id`, `partition`, `subject` | The item, its partition, and the user its process runs as |
| `title`, `rationale` | What to do and why |
| `origin` | `user`, `agent { pid, confidence }`, `consolidation` or `observation { id }` |
| `state` | `proposed` → `approved` → `scheduled { wake }` → `running { pid }` → `done`; or `rejected`, `expired` |
| `plan` | The steps the proposer produced, if any |
| `budget` | The `RunBudget` of the item's process; its cost cap is reserved against the daily budget |
| `expiresAt`, `when` | When an item not yet running expires; the wake the proposer asked for |
| `answer` | For an `observation` item, the person's answer once given: `confirmed`, `by`, `at` and the `fact` it remembered |

An item's process is a `Proposed` process on the `background` queue, in a session of its own
(`agenda-<id>`), whose parent is the proposing run. Every state change is a
`KernelEvent::Agenda` on the event bus and a `kernel.agenda.*` audit record
(`proposed`, `approved`, `rejected`, `scheduled`, `expired`) naming the item, partition,
origin, approver and budget.

## `propose_followup`

With the `kernel` feature, the harness declares one more tool:

```json
{ "title": "Re-run the tests", "rationale": "The dependency bump lands tonight",
  "when": { "at": "2026-10-24T07:00:00Z" },
  "steps": [{ "id": "1", "title": "Test", "instruction": "cargo test --workspace" }] }
```

It is class `Todo`, so every mode allows it, and its only effect is an agenda item in state
`proposed`. `when` is any wake the kernel knows — `{"at": …}`, `{"cron": {…}}`,
`{"fileChange": {…}}`, `{"runFinished": {"pid": …}}` — and is optional. The follow-up's
session starts in the proposing session's mode, so its process never runs above the mode of
the run that proposed it (K-I7). Without a kernel hosting the harness, the tool answers that
none runs.

## Who approves

`ProactivityPolicy::approval` decides where a new proposal goes:

| `approval` | ReadOnly, Plan | Ask | Auto |
|---|---|---|---|
| `follow_mode` (default) | Held `proposed` | To the approvals inbox | Approved by the policy and scheduled |
| `always_ask` | To the inbox | To the inbox | To the inbox |
| `never` | Held `proposed` | Held `proposed` | Held `proposed` |

An inbox proposal is an `ApprovalRequest` with `tool_name = "kernel:agenda"`, the item as
`args_redacted`, and `allowed_actions = [approve, edit, reject]`; an edit may change the
title, rationale, `when` and expiry, never the origin, partition, subject or budget. The
kernel applies inbox decisions on its next tick. A held item runs only when a person approves
it through the agenda.

> **Important:** an item of origin `consolidation` — proposed by the consolidation tick —
> never starts without a person's approval, in every mode, Auto included (K-I23). Approving
> or scheduling it as a service is refused.

An item of origin `observation` is a question, answered and never scheduled (spec 3.2 §7.10).
It needs a person in every mode, Auto included, and never becomes a process or a model turn:

| Answer | How | Effect |
|---|---|---|
| Confirm | `adk-rust agenda approve ID`, or `approve` in the inbox | `proposed` → `done`; the partition's `ObservationAudience` remembers the item's rationale as a `known` fact with the person as its source |
| Reject | `adk-rust agenda reject ID`, or `reject` in the inbox | `proposed` → `rejected`; nothing is remembered, so a related assumed fact stays assumed |
| None | — | `expired` once `expiresAt` passes |

Its inbox request allows `[approve, reject]`, and `adk-rust agenda schedule` on it is refused
with the reason. A question costs no budget, so only `max_proposed_open` refuses one. See
[Answering an observation](proc.md#answering-an-observation).

## Limits

| `ProactivityPolicy` field | Default | Effect |
|---|---|---|
| `max_proposed_open` | 10 | A proposal past this many open items in the partition is rejected |
| `daily_budget` | unlimited | The proactive processes of a partition spend at most its cost cap per UTC day; a proposal once it is spent, or whose own budget exceeds it, is rejected |
| `quiet_hours` | none | No proactive or maintenance process starts inside the window |
| `approval` | `follow_mode` | As above |
| `consolidation` | none | The consolidation tick; the CLI turns it on in its Ask and Auto partitions |
| `review_period` | 90 days | What the daily review reports as old, below |

A rejected proposal is never stored: the bus carries it as `proposed` → `rejected` with the
reason, and `propose_followup` answers the agent with it. Quiet hours and budgets also hold
the item's process at admission; see [Scheduling](scheduling.md).

## The daily review

Once a UTC day the kernel's maintenance tick reviews what may have drifted, per partition
(spec 3.2 §7.4, §9.1):

| List | What |
|---|---|
| `rules` | Standing rules with no expiry granted longer ago than `review_period`, with an ask to renew each with an expiry or revoke it |
| `sleepers` | Processes `Waiting` — asleep, suspended or parked — for longer than the period |
| `frozen` | Processes a freeze parked longer ago than the period |
| `crossings` | Crossings out of the partition that recur at least 5 times in 7 days, with partition membership as the remedy ([partitions](partitions.md#recurring-crossings)) |

A non-empty review is a `KernelEvent::Review` on the bus and one `kernel.review` audit record
with ids, ages, resource names and counts only. The review never revokes a rule or wakes, suspends or kills a process;
a person decides. The kernel reads standing rules through `StandingRuleSource`, which
`Harness::kernel` implements over the harness's rule store
(`KernelBuilder::standing_rules`).

## From code

```rust
use std::sync::Arc;

use adk_core::principal::SubjectRef;
use adk_core::{InMemoryRunStore, RunBudget};
use adk_kernel::{
    AgendaItem, AgendaOrigin, Kernel, KernelConfig, PartitionMode, ProactivityPolicy,
    ProposalOutcome, QuietHours,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let policy = ProactivityPolicy::default()
        .with_daily_budget(RunBudget::new().max_cost_usd(2.0))
        .with_quiet_hours("22:00-07:00".parse::<QuietHours>()?);
    let kernel = Kernel::builder()
        .config(KernelConfig::default().with_proactivity(policy))
        .run_store(Arc::new(InMemoryRunStore::new()))
        .build()?;

    let item = AgendaItem::new(
        "tidy-changelog",
        SubjectRef::human("alice"),
        "Tidy the changelog",
        "Three fragments overlap",
        AgendaOrigin::User,
        chrono::Utc::now(),
    );
    match kernel.agenda().propose(item, PartitionMode::Plan).await? {
        ProposalOutcome::Held { item } => println!("{} waits for a person", item.id),
        ProposalOutcome::Rejected { reason } => println!("refused: {reason}"),
        other => println!("{other:?}"),
    }
    let approved = kernel.agenda().approve("tidy-changelog", &SubjectRef::human("alice")).await?;
    println!("{:?}", approved.state);
    Ok(())
}
```

| Call | Effect |
|---|---|
| `Agenda::propose(item, mode)` | Checks the policy, stores the item `proposed` and routes it |
| `Agenda::approve(id, by)` | `proposed` → `approved` → `scheduled`; withdraws the item's inbox request |
| `Agenda::reject(id, by, reason)` | `proposed` or `approved` → `rejected` |
| `Agenda::schedule(id, wake, by)` | Puts the item's process to sleep on `wake`, approving a proposed item first |
| `Agenda::list(filter)`, `get(id)` | The items, oldest first |

The items live in an `AgendaStore`: `InMemoryAgendaStore`, `SqliteAgendaStore` (in
`harness.db`), `PostgresAgendaStore` (feature `postgres`) or `RedisAgendaStore` (feature
`redis`), or, Alpha, `MongoAgendaStore`, `FirestoreAgendaStore` or `Neo4jAgendaStore`
(features `mongodb`, `firestore`, `neo4j`; [backends](backends.md)), set with
`KernelBuilder::agenda_store`. Every backend passes
`agenda_store_conformance_tests!` (feature `conformance`); `transition` is a compare-and-set on
the item's state, so two replicas approving one item move it once.

## Over the control plane

| Route (under `/api`) | Effect |
|---|---|
| `GET /kernel/agenda?partition=&state=&limit=` | The items |
| `POST /kernel/agenda/{id}/approve` | `Agenda::approve` as the caller |
| `POST /kernel/agenda/{id}/reject` | `Agenda::reject`, with an optional `reason` |
| `POST /kernel/agenda/{id}/schedule` | `Agenda::schedule` with the body's `wake` set |

Reading needs an authenticated caller; the three changes need the `operator` scope and write a
`kernel.control` record before they act. `ControlClient::{agenda, approve_item, reject_item,
schedule_item}` call them.
