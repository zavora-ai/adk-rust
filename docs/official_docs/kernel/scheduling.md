# Scheduling, attention and resources

The kernel decides when a process may start (spec 3.2 §7.9). Priority is a queue, each
partition is bounded by quotas, proactive work waits out quiet hours and stays within the
day's budget, and a human turn preempts background work when its partition is full.

| Mechanism | What the kernel does |
|---|---|
| Priority classes | One `DurableWorker` per queue — `interactive` (500 ms fallback poll), `background` (2 s), `maintenance` (5 s) — so a human turn never queues behind background work; goals run on `background` (`Harness::with_goal_queue`) |
| Quotas | Per partition: `max_running` and `max_queued` processes, a `daily_spend_cap`, and `daily_caps` per usage unit, enforced at admission |
| Preemption | An interactive process that finds its partition at `max_running` takes the slot of the youngest `background` process, which parks at its next model-call boundary and is requeued when the interactive one finishes |
| Quiet hours | Proactive and maintenance processes never start inside the window; interactive and approval-woken runs always may |
| Cost-aware admission | A background or proactive process starts only while the partition's spend today, plus its budget's cost cap, stays within the cap, and its usage of every capped unit today is below that unit's cap; otherwise it sleeps until `BudgetReplenished` |
| Fairness | The quotas bound every partition within a queue, so none starves another; nothing orders claims across partitions |

## Admission

Every claimed process passes admission before its first model call. `kernel.wake` runs, which
route wakes, are never counted or held.

| Class | Processes | Held by |
|---|---|---|
| Interactive | `ProcessKind::Interactive`, or any run on the `interactive` queue | `max_running` (and then it preempts) |
| Background | Goals, scheduled and triggered processes | `max_running`, `daily_spend_cap`, `daily_caps` |
| Proactive | Agenda follow-ups (`Proposed`) and the kernel's maintenance processes | `max_running`, `daily_spend_cap`, `daily_caps`, the daily proactive budget, quiet hours |

A process admission holds sleeps `Waiting`, with no lease and no model call, on a wake set
that says why, and carries the label `kernel.admission`:

| Held for | Wake set | Label | Woken |
|---|---|---|---|
| Quiet hours | `at` the end of the window | `quiet_hours` | By the scheduler tick when the window ends |
| Budget | `budgetReplenished { partition }` | `budget` | When the partition's budget has room again: a new UTC day, or a raised cap |
| A slot | `manual` | `quota` | When a process of its partition parks or finishes; human turns first, then oldest first |

An agenda item the kernel schedules during quiet hours is enqueued with `not_before` at the
window's end, so its worker does not claim it until then. Admission runs again at every
claim, so a process woken into a full partition or a spent budget sleeps again.

A preempted process shows `suspended: preempted by <pid>` and a `runFinished` wake set in
`adk-rust ps`. It makes no further model call, so it stops counting against the quota at once;
the tool calls it already started finish, and the action ledger keeps any of them from running
twice when it resumes. `unfreeze` never wakes it.

## Configuration

```rust
use std::sync::Arc;

use adk_core::{InMemoryRunStore, RunBudget, UsageUnit};
use adk_kernel::{Kernel, KernelConfig, ProactivityPolicy, QuietHours, Quotas};

fn main() -> anyhow::Result<()> {
    let config = KernelConfig::default()
        .with_quotas(
            Quotas::new()
                .with_max_running(3)
                .with_max_queued(20)
                .with_daily_spend_cap(5_000_000) // micro-USD: 5 USD per partition per day
                .with_daily_cap(UsageUnit::Images, 200), // generated images per partition per day
        )
        .with_quiet_hours("22:00-07:00".parse::<QuietHours>()?)
        .with_proactivity(
            ProactivityPolicy::default().with_daily_budget(RunBudget::new().max_cost_usd(1.0)),
        );
    let kernel = Kernel::builder()
        .config(config)
        .run_store(Arc::new(InMemoryRunStore::new()))
        .build()?;
    println!("{:?}", kernel.config().quotas);
    Ok(())
}
```

| Setting | Default | Notes |
|---|---|---|
| `KernelConfig::quotas` | none | Applies to each partition separately; partitions with their own quotas are phase 3 |
| `KernelConfig::quiet_hours` | none | Applies beside `ProactivityPolicy::quiet_hours`; a process waits out both |
| `QuietHours` | — | `HH:MM-HH:MM` in the host's time zone, or `with_utc_offset_minutes`; a start after the end wraps midnight; equal times make no window |
| `Quotas::daily_caps` | none | Per `UsageUnit` (`images`, `audioMillis`, `inputTokens`, …; `dailyCaps` in JSON): the partition's daily usage of the unit; reached means held |
| `ProactivityPolicy::daily_budget` | unlimited | Its `max_cost_micro_usd`, `max_model_calls` and `max_total_tokens` cap the partition's proactive spend, model calls and tokens per UTC day |

Spend and usage come from the kernel's own daily totals per process and partition
(`ProcessStore::usage`: cost, model calls, total tokens and units), the same totals
`adk-rust ps` shows, so admission needs no `SpendLedger`. The kernel attributes them from the
hosted agent's `RunUsage`, which the harness reads from each run's `BudgetTracker`, so media
calls metered in a run ([Calls billed in units](../security/spend-ledger.md#calls-billed-in-units))
count. A proposal is refused while any cap of the daily proactive budget is reached.

## Spawning past `max_queued`

`ProcessTable::spawn`, `POST /api/kernel/processes` and agenda scheduling refuse a process
when its partition already queues `max_queued` processes, with `kernel.quota`
(`ResourceExhausted`, HTTP 429). Runs enqueued outside the kernel, such as through
`/api/runs`, are not refused; admission still holds them at `max_running`.
