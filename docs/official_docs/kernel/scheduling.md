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
| Quiet hours | `at` the end of the window | `quiet_hours` | By the scheduler tick when the window ends, or at once when the area's quiet time or its partition record's window changes over the control plane |
| Budget | `budgetReplenished { partition }` | `budget` | When the partition's budget has room again: a new UTC day, or a cap raised or removed. A change of the area's spending limit or of the partition record over the control plane wakes it at once; any other change — `Partitions::put`, the kernel's quotas or daily proactive budget at a restart — at the next scheduler tick |
| Its agent's daily limit | `at` the next UTC midnight | `the agent's daily limit` | The next UTC day, or at once when the added agent's daily limit changes |
| A slot | `manual` | `quota` | When a process of its partition parks or finishes; human turns first, then oldest first |

An agenda item the kernel schedules during quiet hours is enqueued with `not_before` at the
window's end, so its worker does not claim it until then. Admission runs again at every
claim, so a process woken into a full partition or a spent budget sleeps again. A held process
is decided on again at whatever claim reaches it: a change wakes held work that is still on
its way to park with a pending wake, and the claim the moment it parks admits it once.

Each budget wake is one `kernel.wake` run keyed `budget:<partition>:<UTC day>:<asleep>`, where
`asleep` tags the processes asleep on it and their claims: the work asleep now gets one wake,
and work held again later the same day, after a second raise, gets its own.

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
| `Quotas::daily_caps` | none | Per `UsageUnit` (`images`, `frames`, `audioMillis`, `inputTokens`, …; `dailyCaps` in JSON): the partition's daily usage of the unit; reached means held |
| `ProactivityPolicy::daily_budget` | unlimited | Its `max_cost_micro_usd`, `max_model_calls` and `max_total_tokens` cap the partition's proactive spend, model calls and tokens per UTC day |

Spend and usage come from the kernel's own daily totals per process and partition
(`ProcessStore::usage`: cost, model calls, total tokens and units), the same totals
`adk-rust ps` shows, so admission needs no `SpendLedger`. The kernel attributes them from the
hosted agent's `RunUsage`, which the harness reads from each run's `BudgetTracker`, so media
calls metered in a run ([Calls billed in units](../security/spend-ledger.md#calls-billed-in-units))
count. A hosted agent with no `ProgressSource` usage — any `Arc<Runner>` given to
`KernelBuilder::agent` — is attributed from the usage its turn's model responses report
(`usage_metadata`: cost, tokens, input and output tokens), counted as the worker reads the turn's
events, so quotas and the daily cap hold for a kernel built from code too. A proposal is refused
while any cap of the daily proactive budget is reached.

## Spawning past `max_queued`

`ProcessTable::spawn`, `POST /api/kernel/processes` and agenda scheduling refuse a process
when its partition already queues `max_queued` processes, with `kernel.quota`
(`ResourceExhausted`, HTTP 429). Runs enqueued outside the kernel, such as through
`/api/runs`, are not refused; admission still holds them at `max_running`.

## Cron wakes and missed occurrences

A cron wake, `Wake::Cron { expr, missed }`, fires on a six-field cron expression, seconds
first, in UTC (spec 3.2 §7.4). A standing trigger on one starts a process at each occurrence,
as its [overlap](#overlapping-firings) allows; a process asleep on one wakes at each
occurrence, and its successor sleeps on the same wake. An occurrence that comes due while a
kernel watches the expression, and reaches the router on time, fires. `missed`, an
`adk_core::MissedFirePolicy`, decides the occurrences that came due while the kernel could not
act on them. It has no default, so every cron wake names one:

| `missed` | JSON | Missed occurrences that fire |
|---|---|---|
| `MissedFirePolicy::Skip` | `"skip"` | The latest, only when it is no more than a minute late (`DEFAULT_MISFIRE_GRACE`) |
| `MissedFirePolicy::FireOnce` | `"fireOnce"` | The latest, however late |
| `MissedFirePolicy::CatchUp(n)` | `{"catchUp": n}` | The latest `n`, oldest first; a trigger or a managed wake set with `CatchUp(0)` is refused |

The policy holds however the occurrences were missed:

| Missed because | What the kernel does |
|---|---|
| No kernel watched the expression: a restart or an outage | The expression's cursor replays the occurrences the policy picks, ranked from the latest |
| The kernel was frozen | Each occurrence still goes to the maintenance queue as its `kernel.wake` run, which the freeze holds. After `unfreeze` the router ranks each one behind the later occurrences already due, and fires what the policy picks: under `fireOnce`, one run for the whole freeze |
| Admission holds the trigger's run (a daily cap or the agent's daily limit, a quota) | The held run stands for the occurrences after it: the trigger keeps at most as many runs waiting to start as the policy fires (one for `skip` and `fireOnce`, `n` for `catchUp(n)`), and a later occurrence joins the newest ([`Coalesced`](#overlapping-firings)) |

The router measures how late an occurrence is when it routes it, on the kernel's clock, from its
scheduled time or, with [jitter](#jitter), from its release. An occurrence it passes over
starts nothing and is published as a `missed` wake. Quiet hours hold proactive work only, so
they never hold a trigger's run; a sleeper that admission holds is not asleep on its cron wake,
so the occurrences while it is held wake nothing and it runs once when released.

```rust
use std::sync::Arc;

use adk_core::{InMemoryRunStore, MissedFirePolicy, NewRun};
use adk_kernel::{Kernel, KernelConfig, Trigger, Wake};
use serde_json::json;

fn main() -> anyhow::Result<()> {
    let digest = NewRun::new(
        "background",
        "ops",
        "ana",
        "ops",
        json!({ "text": "Summarize last night's CI failures" }),
    );
    let kernel = Kernel::builder()
        .config(KernelConfig::default().with_app_name("ops"))
        .run_store(Arc::new(InMemoryRunStore::new()))
        // 08:00 UTC on weekdays; after downtime, the latest missed morning runs once.
        .trigger(Trigger::new(
            "ci-digest",
            Wake::Cron { expr: "0 0 8 * * Mon-Fri".into(), missed: MissedFirePolicy::FireOnce },
            digest,
        ))
        .build()?;
    println!("{:?}", kernel.inventory().wake_sources);
    Ok(())
}
```

A sleeper takes the same wake in its wake set:
`ProcessSpec::new(ProcessKind::Scheduled).with_wake(WakeSet::any_of([Wake::Cron { … }]))`.
In JSON — an entry of a wake set's `anyOf`, such as a managed wake set's or the `wake` of
`POST /api/kernel/processes`, or the `when` of `propose_followup` — a cron wake is
`{"cron": {"expr": "0 0 8 * * Mon-Fri", "missed": "fireOnce"}}`. `adk-rust wake --cron` and
`WakeSetDef::cron` use `Skip`.

The kernel keeps each expression's cursor in its `CronWatermarkStore`, so a restart or a
second replica never fires an occurrence twice. The sleepers on one expression are planned
with the widest policy among them, and so are the triggers; each wakes only for the
occurrences its own policy picks ([cron sleepers](backends.md#cron-sleepers),
[cron triggers](backends.md#cron-triggers)).
A durable `Schedule` takes the same `MissedFirePolicy`
([schedules and the firing rule](../core/durable-runtime.md#schedules-and-the-firing-rule)).

## Overlapping firings

A standing trigger's firing can come while a process it started is unfinished: queued,
running, waiting for a person's approval, asleep, held by admission or parked by a freeze. Its
`overlap`, a `WakeOverlap`, decides what the firing does. Every firing follows it — a cron
occurrence, a webhook or queue delivery, a file change, a signal, and a
[run now](#running-a-schedule-now):

| `overlap` | JSON | A firing while one of the trigger's processes is unfinished | Wake outcome |
|---|---|---|---|
| `WakeOverlap::Skip` (default) | `"skip"` | Starts nothing | `skipped`, naming the unfinished process |
| `WakeOverlap::QueueOne` | `"queueOne"` | Queues one process behind it; while one already waits to start — queued, or held by admission — joins that one | `spawned`, then `coalesced` naming the waiting process |
| `WakeOverlap::Queue` | `"queue"` | Queues a process behind it, every time | `spawned` |

`skip` is the default because a scheduled run that waits behind an unfinished one repeats its
work when it finally runs: a weekday status run whose `publish_status` waits a week for a person
would otherwise leave five stale runs to run back to back. The next occurrence after the
unfinished process ends fires as usual. `skip` is also the default of a managed wake set and of
an `adk-server` cron job's `concurrencyPolicy`. A trigger whose every delivery carries work that
must not be dropped — one run per webhook delivery or queue message — sets `queue`.

```rust
use std::sync::Arc;

use adk_core::{InMemoryRunStore, MissedFirePolicy, NewRun};
use adk_kernel::{Kernel, Trigger, Wake, WakeOverlap};
use serde_json::json;

fn main() -> anyhow::Result<()> {
    let refresh =
        NewRun::new("background", "ops", "ana", "ops", json!({ "text": "Refresh the dashboard" }));
    let kernel = Kernel::builder()
        .run_store(Arc::new(InMemoryRunStore::new()))
        .trigger(
            Trigger::new(
                "hourly-refresh",
                Wake::Cron { expr: "0 0 * * * *".into(), missed: MissedFirePolicy::FireOnce },
                refresh,
            )
            // A refresh that overruns the hour runs once more right after, never twice.
            .with_overlap(WakeOverlap::QueueOne),
        )
        .build()?;
    println!("{:?}", kernel.inventory().triggers);
    Ok(())
}
```

A skipped or coalesced firing is visible where wakes are:

| Where | What it shows |
|---|---|
| `adk-rust events` | `trigger-cron:… cron:…:09:00:00Z → <pid> (skipped: still unfinished)`, `(coalesced into it)`, or `→ nothing (missed: too late)` |
| `adk-rust ps` | The process's note counts them: `waiting for a human · 3 later firing(s) skipped`, `2 later firing(s) coalesced into it`, from its `kernel.skipped` and `kernel.coalesced` labels |
| The audit trail | The `kernel.wake` record carries `overlap: skipped` or `coalesced`, or `missed: true`, with the trigger and the process |
| `kernel.inventory()` | Each trigger's `overlap` |

A cron occurrence keeps its cursor and key whatever the overlap does with it: a skipped or
coalesced occurrence is accounted for, so a restart never replays it.

## Jitter

Many schedules on one cadence — every agent on `0 0 * * * *` — would all start in the same
second. `KernelConfig::with_cron_jitter(Jitter)` spreads them: each cron tick and each
schedule fire is delayed by a share of the gap to the next one. Jitter is off by default.

| Field | Meaning |
|---|---|
| `percent` | The share of the gap to the next occurrence a fire may be delayed by, 0 to 50; a larger value is clamped to 50 |
| `max` | The longest delay, whatever the gap |
| `seed` | Folded into every offset; the kernel salts it with its `app_name`, so tenants and kernels whose sources share names spread apart |

`Jitter::default()` is 15 percent, at most nine minutes. The offset of a source is the same on
every replica and after every restart: it is a fixed fraction of the window, derived from the
source's name. It is always under half the gap, so a delayed tick lands before the next one
is due.

A jittered tick keeps its scheduled time as its identity. The kernel submits it when it falls
due, keyed `cron:<source>:<scheduled time>`, and moves the expression's watermark to that
time, as without jitter; the tick's `kernel.wake` run carries `not_before`, the scheduled time
plus the offset, so the maintenance worker routes it no earlier. The missed-occurrence plan,
the deduplication across replicas and the catch-up after downtime are therefore unchanged. The
woken turn's wake payload carries `jitter_ms`.

```rust
use std::sync::Arc;
use std::time::Duration;

use adk_core::schedule::Jitter;
use adk_core::{InMemoryRunStore, MissedFirePolicy, NewRun};
use adk_kernel::{Kernel, KernelConfig, Trigger, Wake};
use serde_json::json;

fn main() -> anyhow::Result<()> {
    let digest =
        NewRun::new("background", "ops", "ana", "ops", json!({ "text": "Summarize the hour" }));
    let kernel = Kernel::builder()
        .config(
            KernelConfig::default()
                .with_app_name("ops")
                // Up to 15 percent of the hour, at most nine minutes.
                .with_cron_jitter(Jitter::new(15, Duration::from_secs(9 * 60))),
        )
        .run_store(Arc::new(InMemoryRunStore::new()))
        .trigger(Trigger::new(
            "hourly-digest",
            Wake::Cron { expr: "0 0 * * * *".into(), missed: MissedFirePolicy::Skip },
            digest,
        ))
        .build()?;
    println!("{:?}", kernel.inventory().wake_sources);
    Ok(())
}
```

The kernel applies the jitter to its own cron sources for standing triggers, to the cron
wakes of sleepers, to a managed tenant's cron sources and to the `DurableScheduler` over its
schedule store. A `CronWakeSource` you register takes its own:
`CronWakeSource::watermarked(..)?.with_jitter(Jitter::default())`.

## Running a schedule now

`Kernel::run_now` runs a cron schedule now, outside its schedule, for a person or a program:

| Target | `RunNowTarget` | Control plane | CLI |
|---|---|---|---|
| A process asleep on a cron wake | `Process { pid }` | `POST /api/kernel/processes/{pid}/run-now` | `adk-rust run-now PID` |
| A standing trigger on a cron wake | `Trigger { name }` | `POST /api/kernel/triggers/{name}/run-now` | `adk-rust run-now --trigger NAME` |

A run now is a wake like a tick's, keyed `now:<pid or trigger:name>:<request id>`: the
router wakes the sleeper, or fires the trigger, as a tick would. The request body,
`{"requestId": "…"}`, is optional; a retry with the same id queues nothing new. The run now:

- **is refused while frozen** with `kernel.frozen`, and queues nothing;
- **follows the trigger's [overlap](#overlapping-firings)** as an occurrence does: while a
  process the trigger started is unfinished, `skip` refuses the run now with
  `kernel.invalid_state` naming that process, `queueOne` queues one run behind it or joins the
  one already waiting, and `queue` queues one behind it;
- **passes admission** like a tick's run: quiet hours defer it, a spent daily budget parks it,
  a quota holds it;
- **leaves the schedule alone**: no watermark moves, the next tick fires on time, and a
  recurring sleeper's successor sleeps on the same cron wake after the run;
- **tells the turn** it was run now: the wake payload carries `run_now`, the expression and
  `requested_by`;
- **is recorded** as a `kernel.control` record of the `run-now` action and a `kernel.run_now`
  record with the target, the key and the `kernel.wake` run.

A process that is not asleep on a cron wake is refused with `kernel.invalid_state`;
`adk-rust resume` wakes any sleeper. The routes need the `operator` scope.

