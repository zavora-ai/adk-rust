# adk-kernel

The Agent Kernel of ADK-Rust: a resident supervisor that keeps agents as durable processes —
asleep at no cost, woken by what matters, frozen everywhere with one switch — over the
durable-run machinery of `adk-core` and `adk-server`
(spec 3.2, [`docs/design/spec-3.2-agent-kernel.md`](https://github.com/zavora-ai/adk-rust/blob/main/docs/design/spec-3.2-agent-kernel.md)).

[![Crates.io](https://img.shields.io/crates/v/adk-kernel.svg)](https://crates.io/crates/adk-kernel)
[![Documentation](https://docs.rs/adk-kernel/badge.svg)](https://docs.rs/adk-kernel)
[![License](https://img.shields.io/crates/l/adk-kernel.svg)](https://github.com/zavora-ai/adk-rust/blob/main/LICENSE)

A process is a durable run. Asleep is `Waiting` with a recorded wake set and no lease, every
wake is a governed run, and the kernel owns the state that terminals attach to. `adk-kernel`
hosts any `Arc<Runner>` and depends on no harness type; the `kernel` feature of `adk-harness`
builds a kernel from a harness (`Harness::kernel`).

| Part | What the kernel runs |
|---|---|
| Workers | One `DurableWorker` per `QueueClass`: `interactive` (500 ms poll), `background` (2 s), `maintenance` (5 s). A run refused by a freeze parks `Waiting` instead of failing |
| Scheduler | `DurableScheduler` over a `ScheduleStore`, every `KernelConfig::scheduler_tick` (1 s) on `KernelConfig::clock` |
| Process table | `ProcessTable`: `ps`, `get`, `spawn`, `kill`, `nice`, `suspend`, `resume`. Each process is a run, a `ProcessMeta` row in a `ProcessStore`, and a live `ProcessView` |
| Wakes | `WakeSet`s on sleeping processes and standing `Trigger`s, matched by the `WakeRouter`; every event becomes one `kernel.wake` run keyed by the event, so a redelivery wakes nothing |
| Event bus | `KernelEvent`s to subscribers, with replay from a cursor, and to `EventSink`s such as `AuditSink` |
| Kill switch | `Kernel::freeze` parks every running process on every replica; `Kernel::unfreeze` resumes exactly the processes the freeze parked |
| Control plane | `control::router` on a Unix socket and loopback HTTP, merged into `ServerBuilder` by `adk-rust up`; `ControlClient` for terminals |

## Installation

```toml
[dependencies]
adk-kernel = "3.0.0"
```

## Example

A kernel over one SQLite file runs a Gemini agent. One process sleeps until `Cargo.lock`
changes; the kernel drains and stops on `SIGTERM` or Ctrl-C.

```rust,ignore
use std::sync::Arc;
use std::time::Duration;

use adk_agent::LlmAgentBuilder;
use adk_core::NewRun;
use adk_kernel::{
    Kernel, KernelConfig, ProcessKind, ProcessSpec, QueueClass, SqliteProcessStore, Wake, WakeSet,
};
use adk_model::gemini::GeminiModel;
use adk_runner::Runner;
use adk_session::{SessionService, SqliteRunStore, SqliteSessionService};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let sessions = Arc::new(SqliteSessionService::new("sqlite://kernel.db?mode=rwc").await?);
    sessions.migrate().await?;
    let runs = SqliteRunStore::from_pool(sessions.pool().clone());
    runs.migrate().await?;
    let processes = SqliteProcessStore::new(sessions.pool().clone());
    processes.migrate().await?;

    let model = Arc::new(GeminiModel::new(std::env::var("GOOGLE_API_KEY")?, "gemini-3.7-flash")?);
    let agent = LlmAgentBuilder::new("auditor")
        .instruction("Summarise what changed in the lockfile.")
        .model(model)
        .build()?;
    let runner = Arc::new(
        Runner::builder()
            .app_name("adk-rust")
            .agent(Arc::new(agent))
            .session_service(sessions.clone() as Arc<dyn SessionService>)
            .build()?,
    );

    let kernel = Kernel::builder()
        .config(KernelConfig::default())
        .run_store(Arc::new(runs))
        .process_store(Arc::new(processes))
        .session_service(sessions as Arc<dyn SessionService>)
        .agent("adk-rust", runner)
        .build()?;
    kernel.start().await?;

    let wake = WakeSet::any_of([Wake::FileChange {
        root: std::env::current_dir()?,
        glob: "Cargo.lock".into(),
        debounce: Duration::from_millis(500),
    }]);
    let run = NewRun::new(
        QueueClass::BACKGROUND,
        "adk-rust",
        "alice",
        "adk-rust",
        serde_json::json!({ "text": "Audit the lockfile change." }),
    );
    let process = kernel
        .processes()
        .spawn(run, ProcessSpec::new(ProcessKind::Goal).with_wake(wake))
        .await?;
    println!("{} sleeps until Cargo.lock changes", process.meta.pid);

    kernel.shutdown_on_signal().await?;
    Ok(())
}
```

## Processes

| Action | What happens |
|---|---|
| `spawn` | Enqueues the run and records its process. With `ProcessSpec::wake` the process starts asleep: its run's idempotency key carries the sleep intent (`kernel:asleep:`), so it parks before its first model call however late its row is written |
| Asleep | `Waiting` with a wake set: no lease, no model call, no spend (K-I2) |
| Wake | A matching event, a passed `Wake::At`, or an expiry with `ExpiryAction::Run` makes the run claimable; the claim resumes its session |
| `suspend` | A running process stops at its next model-call boundary and parks on its session checkpoint; the replica that holds the lease honours the request within 250 ms |
| `resume` | Wakes a suspended or sleeping process; a freeze-parked one waits for `unfreeze` |
| `kill` | `request_cancel`: cooperative at the next checkpoint for a running process, at once for a queued or waiting one |
| `nice` | Moves a queued or sleeping process to another queue class under a new pid, with `parent` set to the old one; a running process is refused until it is suspended |

`ProcessView` carries the age, spend today and over seven days, the burn rate and the
projection to the end of the day, model and tool calls from the transcript, the loop guard's
verdict, and a goal's progress (iteration of max, spend, duration, last check). Spend comes
from the kernel's own per-process daily totals, fed by a `ProgressSource`.

## Wake sources

| Source | Wake | Event key |
|---|---|---|
| `CronWakeSource` | `Wake::Cron` | `cron:<source>:<scheduled for>` |
| `FileWakeSource` (one per watched root, started when a sleeper needs it) | `Wake::FileChange` | `file:<root>:<path>:<modified>` |
| `webhook_wake_hook` on a `WebhookIngress` | `Wake::Webhook` | `hook:<name>:<delivery id>` |
| `queue_wake_ingress` over a `QueueSource` | `Wake::Queue` | `queue:<source>:<message id>` |
| `ApprovalWakeSource`, an `ApprovalNotifier` | `Wake::Approval` | the approval request id |
| `RunFinishedWakeSource`, a `WorkerHooks` | `Wake::RunFinished` | `run:<pid>:<state>` |
| The tick, on `KernelConfig::clock` | `Wake::At`, wake-set expiry | — |

A `Trigger` spawns a new process for an event no sleeper takes, keyed
`<event key>#<trigger name>`. Changes to watched files while the kernel was down are
reconciled on start. A local event reaches `Running` in under 2 s: in-process sources
enqueue and claim at once, and the tick claims wake runs an external ingress or another
replica enqueued.

## Lifecycle

| Call | Effect | Audit record |
|---|---|---|
| `start` | Reconciles missed wakes, starts the workers, scheduler, wake sources and control plane | `kernel.started` |
| `pause` | Stops claiming; running processes finish their attempt | `kernel.paused` |
| `resume` | Claims again and wakes what a drain parked | `kernel.resumed` |
| `drain(timeout)` | Suspends running processes at their next checkpoint, then stops claiming; on timeout the leases lapse | `kernel.drained` |
| `stop` | Drains, stops the sources and closes the control plane | `kernel.stopped` |
| `shutdown_on_signal` | `SIGTERM` or Ctrl-C: `drain(30 s)`, then `stop` | as above |
| `freeze`, `unfreeze` | `GovernanceControl::freeze_recorded` and `unfreeze_recorded` | `governance.freeze`, `governance.unfreeze` |

A hard kill loses nothing: the lease expires, and the next claim resumes the session from
its checkpoint without running a completed call again.

## Replicas

Kernels share one run store, process store and governance store. Give each replica
`KernelConfig::mirrored_governance(store)`, a kill switch that polls the shared state every
250 ms, so a freeze on any replica parks every running process on all of them within a
second.

| Store | Backends |
|---|---|
| `ProcessStore` | `InMemoryProcessStore`; `SqliteProcessStore` (feature `sqlite`, on the pool of `harness.db`); `PostgresProcessStore` (`postgres`); `RedisProcessStore` (`redis`) |
| Runs, sessions, schedules, approvals, governance state, action ledger | `adk-session`'s stores on the same backend |

`process_store_conformance_tests!` (feature `conformance`) checks a `ProcessStore` backend
against the contract the kernel relies on.

## Features

| Feature | Default | Pulls in |
|---|---|---|
| `sqlite` | on | `SqliteProcessStore` |
| `postgres`, `redis` | off | `PostgresProcessStore`, `RedisProcessStore` |
| `webhooks` | on | `webhook_wake_hook` over `adk-server`'s `WebhookIngress` |
| `proc` | on | The OpenTelemetry GenAI histograms of the live process aggregates |
| `conformance` | off | The `conformance` module |
| `drivers`, `service` | on | Reserved for supervised MCP drivers and service installation; empty in this release |
| `proc-baselines`, `channels`, `managed` | off | Reserved for later phases of spec 3.2; empty in this release |

## Tests

| Suite | Covers |
|---|---|
| `tests/kernel_scenarios.rs` | K2: ten sleepers on ten kinds of wake set make no model call and hold no lease over a clock-controlled hour. K3: a file save, a signed webhook and a queue message each wake one run in under 2 s, and redeliveries wake nothing. K9: two replicas over one SQLite file, six running processes parked within 1 s of a freeze on both, resumed by the unfreeze with every tool call once. Chaos: a kernel process killed with `SIGKILL` mid-step, restarted, resumes from its checkpoint |
| `tests/supervisor_tests.rs`, `tests/supervisor_property_tests.rs` | The process actions, lifecycle and spend attribution; sleepers never call the model and freezes stop everything within a second, with process-store writes delayed up to 120 ms |
| `tests/wake_*.rs` | Matching, keys, expiry, reconciliation and each wake source |
| `tests/process_store_*.rs` | The conformance suite on every backend; PostgreSQL and Redis run `#[ignore]`d against a server (`scripts/test-postgres.sh`) |

## License

Apache-2.0
