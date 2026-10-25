# adk-kernel

The Agent Kernel of ADK-Rust: a resident supervisor that keeps agents as durable processes —
asleep at no cost, woken by what matters, frozen everywhere with one switch — over the
durable-run machinery of `adk-core` and `adk-server`
(spec 3.2, [`docs/design/spec-3.2-agent-kernel.md`](https://github.com/zavora-ai/adk-rust/blob/main/docs/design/spec-3.2-agent-kernel.md)).

[![Crates.io](https://img.shields.io/crates/v/adk-kernel.svg)](https://crates.io/crates/adk-kernel)
[![Documentation](https://docs.rs/adk-kernel/badge.svg)](https://docs.rs/adk-kernel)
[![License](https://img.shields.io/crates/l/adk-kernel.svg)](https://github.com/zavora-ai/adk-rust/blob/main/LICENSE)

## What it is

The kernel is a background program, started with `adk-rust up`, that keeps your agents
running when no terminal is open. It does for agents what an operating system does for
programs: it starts them, lets them sleep, wakes them when something happens, makes them take
turns, and stops all of them with one switch.

### A day with the kernel

```bash
# 1. Start the kernel in a project. In ask mode, edits and commands wait for your approval.
adk-rust up --mode ask

# 2. In another terminal, give an agent a job that waits for something to happen.
#    It becomes a sleeping process: no lease, no model calls, no cost while it waits.
adk-rust wake --file notes.md -- "summarise what changed in notes.md"
adk-rust ps

# 3. Save notes.md. The kernel wakes the process; it reads the file and does the job.

# 4. A command the agent wants to run waits in the approvals inbox. Decide it from any
#    terminal, or from another machine with --server and --token against a kernel served
#    with --auth-token.
adk-rust approvals list
adk-rust approvals approve <id>

# 5. Follow-up work the agent suggests lands on the agenda and runs only once you approve it.
adk-rust agenda
adk-rust agenda approve <id>

# 6. One switch parks every running process at a safe point; unfreeze resumes them where they
#    stopped, and a call with side effects that already completed does not run again.
adk-rust freeze
adk-rust unfreeze

# 7. Once there is new material and the project has been idle a while (never inside quiet
#    hours), a consolidation tick merges duplicate memories and expires stale guesses. It never
#    changes a fact you confirmed, and a tick can be reversed.
adk-rust memory undo <tick-id>
```

### The parts, in plain terms

| Part | What it does |
|---|---|
| Processes | Each agent job is a process you list (`ps`), watch (`top`), suspend, resume, re-prioritise (`nice`) or `kill` |
| Wake sources | What wakes a sleeping process: a file change, a cron schedule, a webhook, an approval being decided, another process finishing |
| Scheduler | Three queues — your typed turns first, then background work, then maintenance. Quotas, a daily budget and quiet hours bound background work, and a typed turn takes a busy slot from it |
| Agenda | Where an agent's proposed follow-ups wait for your decision. Proposals from the consolidation tick, and the questions `/proc` asks about your routine, need a person in every mode, including `auto` |
| Kill switch | `freeze` parks running processes on every replica sharing the store; `unfreeze` resumes exactly the ones it parked |
| Control plane | How terminals and apps reach the kernel: an owner-only local socket, plus an HTTP API on loopback, or beyond it with authentication |
| Events | What the kernel does — process changes, wakes, decisions, drivers, spend — as a stream you follow with `adk-rust events`; a webhook sink posts the ones you select, signed, to an endpoint of your own |
| Drivers | MCP servers the kernel runs for your agents under a manifest you approved: mounted with `adk-rust drivers mount`, or listed under `[kernel] drivers` in `harness.toml` and approved when `up` starts. Each runs in the OS sandbox unless its manifest says otherwise, gets its secrets as files, and a tool whose declaration changes waits until you re-approve it |
| Service | `adk-rust up --install` makes the kernel a service under your account that the system starts and restarts on failure: a launchd agent on macOS, a systemd user unit on Linux, a Windows service; `--uninstall` removes it |
| Self-observation (`/proc`) | A content-free ledger of what your agents did — kinds, durations and hashed subjects, never what they read or wrote. Once a day the kernel compares your routine with its last four weeks and, when it changes for long enough, asks you about it in plain words (`adk-rust proc`) |
| Partitions | Separate compartments, such as work and home, each run by an agent of its own with its own memory, drivers and mode. Nothing crosses between them except through the coordinator, and only with your approval |
| Stores | Processes, schedules, the agenda and the ledger live in SQLite (`harness.db`) or a shared PostgreSQL or Redis backend, so a crash or restart resumes work instead of losing it |

Every tool call a kernel process makes still goes through the harness's modes, standing rules
and approvals. What spec 3.2 still lists as open is under its
[Remaining work](https://github.com/zavora-ai/adk-rust/blob/main/docs/design/spec-3.2-agent-kernel.md#remaining-work).

## How it is built

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
| Event bus | `KernelEvent`s to subscribers, with replay from a cursor, and to `EventSink`s such as `AuditSink` and `WebhookSink`, which posts the events an `EventFilter` selects to an endpoint with Standard Webhooks signatures, bounded retry and dead letters |
| Kill switch | `Kernel::freeze` parks every running process on every replica; `Kernel::unfreeze` resumes exactly the processes the freeze parked |
| Control plane | `control::router` on a Unix socket (a named pipe on Windows) and loopback HTTP, merged into `ServerBuilder` by `adk-rust up`; `ControlClient` for terminals |
| Service | `service::install` keeps `adk-rust up` running as the user: a launchd agent, a systemd user unit or a Windows service, logging to `<home>/kernel.log` and restarting on failure |
| Agenda | `Kernel::agenda`: proposals (`propose`, `approve`, `reject`, `schedule`) under a `ProactivityPolicy` — held, put to the approvals inbox, or scheduled by the partition's mode, bounded by open items, a daily budget and quiet hours; the harness's `propose_followup` lands here |
| Admission | Before a claimed process's first model call: per-partition `Quotas`, `QuietHours`, the day's spend, and preemption of the youngest background process by a human turn at quota |
| Consolidation tick | With `ProactivityPolicy::consolidation` and a `Consolidator` per partition (`KernelBuilder::consolidator`), a `Maintenance` process (`kernel.consolidation`) on the `maintenance` queue once `admit_tick` holds: new material, `min_interval`, `debounce` idle, outside quiet hours, within `budget_share` of the daily proactive budget and `max_tokens_per_tick`; its proposals are `Consolidation` agenda items a person approves in every mode |
| Drivers | `drivers::DriverSupervisor`: MCP servers run as supervised child processes under the manifests the user approved, in the OS sandbox, with brokered secrets; a changed tool declaration needs re-approval, and a call parked on its server's question survives a restart by re-issue |

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
reconciled on start. `CronWakeSource::watermarked` keeps where a cron source left off in a
`CronWatermarkStore` (in-memory, SQLite, PostgreSQL, Redis, MongoDB, Firestore and
Neo4j; `cron_watermark_store_conformance_tests!`), so the ticks missed while no kernel watched fire
once each, as the wake's `MissedFirePolicy` picks them. A sleeper's cron wake keeps its cursor in
the kernel's own store (`KernelBuilder::watermark_store`), so a restarted kernel or another replica
fires the occurrences missed while no kernel ran, one at a time as each series sleeps again, and
none from before a series began. A local event reaches `Running` in under 2 s: in-process sources
enqueue and claim at once, and the tick claims wake runs an external ingress or another
replica enqueued.

## Partitions

A `Partition` is a policy class, a memory scope and a driver set, run by its own harness
(spec 3.2 §7.8). `Partitions` keeps the table and the crossing ledger in a `PartitionStore`
(in-memory, SQLite, PostgreSQL, Redis, MongoDB, Firestore and Neo4j;
`partition_store_conformance_tests!`).

| Part | What it does |
|---|---|
| `Partition::identity_source` | Every agent of the partition's runner acts with the partition's class and tenant, so `PolicyRule::for_class` rules apply |
| `Partition::scope_memory` | Confines an `adk_core::Memory` to the partition's project |
| `PartitionPolicy` | Denies a request reaching another partition unless the coordinator makes it, then requires a non-waivable approval; an approved read holds for the rest of its run, keyed by the crossing fingerprint (the call's fingerprint with the target partition and resource class) |
| `CrossingTool` | Offers the coordinator another partition's driver or session tool as a crossing |
| `DriverSupervisor::crossing_toolset` | Offers the coordinator other partitions' drivers as crossings `<partition>__<tool>`, read from the table at every listing and judged by the driver policy as the tool each wraps |
| `KernelBuilder::partition_agent` | Hosts a partition's runner as a target bound to its partition: a process of another partition cannot run it or continue its sessions |
| `KernelBuilder::partition_progress_source` | The partition's own progress source: the kernel reads its processes' goal progress, loop-guard verdicts and usage from it, by the process's partition, and hands it the process table and the ledger; a partition without one falls back to the main source |
| Managed form | `KernelDef::partitions` and `coordinator` run one agent per partition on a tenant's kernel through `TenantKernelBuilder::partition_resolver`, over the tenant's own partition table (`partition_store`); with `TenantKernelBuilder::drivers`, each driver a partition mounts is proposed in the tenant's inbox and mounted on the tenant's own supervisor when a person approves it |
| Audit | `kernel.partition.created`, `kernel.partition.changed`, and `kernel.partition.crossing` for every crossing, with a digest of its fingerprint |

## Lifecycle

| Call | Effect | Audit record |
|---|---|---|
| `start` | Reconciles missed wakes, starts the workers, scheduler, wake sources and control plane | `kernel.started` |
| `pause` | Stops claiming; running processes finish their attempt | `kernel.paused` |
| `resume` | Claims again and wakes what a drain parked | `kernel.resumed` |
| `drain(timeout)` | Suspends running processes at their next checkpoint, then stops claiming; on timeout the leases lapse | `kernel.drained` |
| `stop` | Drains, stops the sources and closes the control plane | `kernel.stopped` |
| `shutdown_on_signal` | `SIGTERM`, Ctrl-C or a Windows service stop: `drain(30 s)`, then `stop` | as above |
| `freeze`, `unfreeze` | `GovernanceControl::freeze_recorded` and `unfreeze_recorded` | `governance.freeze`, `governance.unfreeze` |

A hard kill loses nothing: the lease expires, and the next claim resumes the session from
its checkpoint without running a completed call again.

## Drivers

A driver is an MCP server the kernel runs under a `DriverManifest` (TOML) the user
approved by mounting it (spec 3.2 §7.7, K-I13, K-I14):

| Piece | Behaviour |
|---|---|
| `DriverManifest`, `EffectClass` | Command, environment, partitions, sandbox, restart policy, and each tool's effect class; unknown tools are `other`; a workspace's manifest may only restrict (`restrict_with`, `load_manifests`) |
| Launcher | `sandbox = "os"` runs the server through `adk_sandbox::sandbox::sandboxed_command`, else records `Unsandboxed { reason }`; the environment is cleared to an allowlist; each `secret://` value is minted by the `CredentialBroker` into a `0600` file named by `<NAME>_FILE` |
| Supervision | One `McpServerManager` per driver; its status channel becomes `KernelEvent::Driver` and `kernel.driver.*` records; crashes restart with backoff; the driver runs in a Job Object on Windows, and leads a session and process group of its own on macOS and Linux, that ends its whole process tree with it, recorded as `DriverRecord::process_tree` |
| Pinning | The first listing after approval pins each tool's declaration digest; a differing one is stale: `NeedsReapproval`, the harness's rules pinned to it stop matching, `DriverPolicy` asks before every call; `reapprove` pins the new declarations and moves the re-approving person's rules |
| Parked calls | A call the server answers with a question parks as a held call, its `DriverPark` stored as the run's checkpoint; the decision, in this process or after a restart, re-issues it once |
| Partitions | `toolset_for(partition, drivers)` serves a driver when its manifest lists the partition and the partition's list names it; `toolset_for_manifests(partition)` by the manifest alone |

```rust,no_run
use std::sync::Arc;
use adk_kernel::drivers::{DriverManifest, DriverSupervisor};
use adk_kernel::stores::InMemoryDriverStore;
use adk_kernel::{Kernel, PartitionId};

# async fn example(runs: Arc<dyn adk_core::DurableRunStore>) -> adk_kernel::Result<()> {
let drivers = DriverSupervisor::builder(Arc::new(InMemoryDriverStore::new())).build();
let kernel = Kernel::builder().run_store(runs).drivers(drivers.clone()).build()?;
kernel.start().await?;
drivers.mount(DriverManifest::from_file("email.toml")?, "alice").await?;
let tools = drivers.toolset_for(&PartitionId::new("work"), &["email".to_string()]);
# let _ = tools;
# Ok(())
# }
```

## Local transport and service installation

| Platform | Control plane | Trust rule | Service form |
|---|---|---|---|
| macOS, Linux | Unix socket `<home>/kernel.sock`, mode `0600` | The peer's uid is the socket owner's | launchd agent, systemd user unit |
| Windows | Named pipe `\\.\pipe\adk-rust-kernel-<tag>`, the tag derived from the home | A protected DACL admits only the user the kernel runs as, who owns the pipe; the token the client opened it with must be that user's; the client refuses a pipe another account owns | Windows service under the user's account |

`ControlClient::connect_local(home)` finds either. `service::ServiceDefinition::render`
shows any platform's definition without installing; `service::install` and
`service::uninstall` act on the current platform, and `service::run_as_service` is the
Windows service's entry point.

## Pairing devices and chat channels

`ControlPlaneConfig::with_pairing(Pairing::new(store))` lets a phone, a tray or a remote
terminal reach the control plane with a device token instead of the operator's credentials
(spec 3.3 §10 K2): `adk-rust pair` issues a ten-minute code, the device claims it at
`POST /api/kernel/pairing/claim` and receives `adkd_<id>_<secret>` once, and the token opens
only the routes its scopes name — `read`, `decide` (low and medium risk), `stop` — until it
expires or `adk-rust pair revoke` removes it, which also ends an event stream it holds open.
Codes (twelve characters, 60 bits) and tokens are stored hashed, and every claim counts
toward a lockout the store shares between replicas; pairing never widens the bind (S-I7).

With the `channels` feature, `channels::ChannelGateway` is the kernel's side of every chat
thread (spec 3.3 §10 K1): an unpaired account gets a pairing code and starts nothing, a
paired account's message is a turn whose reply returns to its thread, the `ChannelDoorbell`
sends decision cards that carry the agent, a masked summary and the risk but never the
arguments, chat answers **Allow once** or **Deny** at low and medium risk through
`ApprovalService::decide` but never a call that moves money, and "stop all" freezes while
"resume" is refused. A platform
adapter implements `ChannelSink` and passes `channel_contract_tests!`; `InMemoryChannel` is
the test channel. See [Pairing devices and chat channels](../docs/official_docs/kernel/pairing.md).

## Settings, while you were away, memory and record

The routes behind the home's Today and "Agent Memory and Rules" (spec 3.3 §10 K3–K5):
`/api/kernel/settings` reads and changes each area's quiet time, daily spending limit and
mode while the kernel runs, from the agents' next step; `/api/kernel/away` is "While you were
away" over the `/proc` ledger with a cursor per device, exact across restarts;
`/api/kernel/memory` lists and corrects what the agents remember through a `MemoryBook`;
`/api/kernel/record` reads the latest actions and checks the signed audit log through a
`RecordBook`. Devices with `read` read them; only the person changes a setting or a memory.
Changed settings and cursors live in the `ControlStore`
(`ControlPlaneConfig::with_control_store`). See
[Settings, while you were away, memory and record](../docs/official_docs/kernel/today-and-memory.md).

## Replicas

Kernels share one run store, process store and governance store. Give each replica
`KernelConfig::mirrored_governance(store)`, a kill switch that polls the shared state every
250 ms, so a freeze on any replica parks every running process on all of them within a
second.

| Store | Backends |
|---|---|
| `ProcessStore` | `InMemoryProcessStore`; `SqliteProcessStore` (feature `sqlite`, on the pool of `harness.db`); `PostgresProcessStore` (`postgres`); `RedisProcessStore` (`redis`) |
| `AgendaStore` | `InMemoryAgendaStore`; `SqliteAgendaStore` (`sqlite`); `PostgresAgendaStore` (`postgres`); `RedisAgendaStore` (`redis`) |
| `DriverStore` | `InMemoryDriverStore`; `SqliteDriverStore`; `PostgresDriverStore`; `RedisDriverStore` (feature `drivers` with the backend's) |
| `pairing::DeviceStore` | `InMemoryDeviceStore`; `SqliteDeviceStore`; `PostgresDeviceStore`; `RedisDeviceStore` |
| `control::ControlStore` | `InMemoryControlStore`; `SqliteControlStore`; `PostgresControlStore`; `RedisControlStore` |
| `channels::ChannelBindingStore` (feature `channels`) | `InMemoryChannelBindingStore`; `SqliteChannelBindingStore`; `PostgresChannelBindingStore`; `RedisChannelBindingStore` |
| Runs, sessions, schedules, approvals, governance state, action ledger | `adk-session`'s stores on the same backend |

`process_store_conformance_tests!`, `agenda_store_conformance_tests!`,
`driver_store_conformance_tests!`, `device_store_conformance_tests!` and
`channel_binding_store_conformance_tests!` (feature `conformance`) check a backend against the
contract the kernel relies on.

### MongoDB, Firestore, Neo4j

The process, agenda, driver, ledger, partition and cron watermark stores also run on MongoDB,
Firestore and Neo4j (spec 3.2 §13 Q8); the device, control and channel binding stores do not.
These backends are **Beta**, as the rest of the crate: each store's conformance suite passes
against a live server in CI's nightly `durable-stores` job, and their storage layout follows the
Beta contract of [STABILITY.md](../STABILITY.md). Each feature also turns on the same backend of
`adk-session`'s run, schedule and governance stores, and every store takes the handle
those stores use, so one client serves them all.

| Store | MongoDB (`mongodb`) | Firestore (`firestore`) | Neo4j (`neo4j`) |
|---|---|---|---|
| `ProcessStore` | `MongoProcessStore` | `FirestoreProcessStore` | `Neo4jProcessStore` |
| `AgendaStore` | `MongoAgendaStore` | `FirestoreAgendaStore` | `Neo4jAgendaStore` |
| `DriverStore` (with `drivers`) | `stores::MongoDriverStore` | `stores::FirestoreDriverStore` | `stores::Neo4jDriverStore` |
| `LedgerStore` (with `proc`) | `proc::MongoLedgerStore` | `proc::FirestoreLedgerStore` | `proc::Neo4jLedgerStore` |
| `PartitionStore` | `MongoPartitionStore` | `FirestorePartitionStore` | `Neo4jPartitionStore` |
| `CronWatermarkStore` | `MongoCronWatermarkStore` | `FirestoreCronWatermarkStore` | `Neo4jCronWatermarkStore` |
| Opened with | `connect(url, database)` or `from_database(db)` | `connect(project)` or `from_db(db)` | `connect(uri, user, password)` or `from_graph(graph)` |

```rust,no_run
use adk_kernel::{MongoAgendaStore, MongoProcessStore};
use adk_session::MongoRunStore;

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let processes = MongoProcessStore::connect("mongodb://localhost:27017", "adk").await?;
let db = processes.database().clone();
let runs = MongoRunStore::from_database(db.clone());
let agenda = MongoAgendaStore::from_database(db);
runs.migrate().await?;
processes.migrate().await?;
agenda.migrate().await?;
# Ok(())
# }
```

`adk-rust serve --store` and `adk-rust up --store` take PostgreSQL and Redis URLs only:
`--store` also keeps the harness's modes and standing rules, which have no MongoDB,
Firestore or Neo4j backend, so a kernel on these backends is built in code with
`KernelBuilder`.

## `/proc`: the activity ledger and baselines

With `KernelBuilder::ledger(Ledger::new(store, key))` the kernel keeps a content-free
activity ledger (feature `proc`): one append-only table of `ts, partition, agent, kind,
effect, duration_ms, subject_hash, meta, trace_id` rows, written for every `KernelEvent` and,
through the `LedgerSink` the kernel hands its `ProgressSource` at start, by the hosted
harness from its tool events and audit records. A row holds labels, counts, durations and an
HMAC of its subject under a `LedgerKey` kept in the kernel home — never a payload, a prompt or
a secret (spec 3.2 K-I19).

| Store | Backends |
|---|---|
| `LedgerStore` | `InMemoryLedgerStore`; `SqliteLedgerStore` (`sqlite`); `PostgresLedgerStore` (`postgres`); `RedisLedgerStore` (`redis`); `MongoLedgerStore` (`mongodb`); `FirestoreLedgerStore` (`firestore`); `Neo4jLedgerStore` (`neo4j`); suite `ledger_store_conformance_tests!` |

With `proc-baselines`, the maintenance tick runs the daily `kernel.proc` pass over
`adk-eval`'s ledger statistics — 28-day median and MAD baselines, a 14-day warm-up, the 7-
and 14-day deviation test, two dimensions or one for ten days, a 7-day cooldown — and
records each change as an `Observation` worded as a neutral question (`LlmPhraser`, or the
kernel's template). `GET /api/kernel/proc/ledger|baselines|observations` serve them. In a
partition with an `ObservationAudience` (`KernelBuilder::observation_audience`; the harness is
one), each observation is also an agenda question, answered and never scheduled: confirmed,
its statement becomes a `Known` fact with the person as its source; rejected, nothing is
remembered. See [Self-observation: `/proc`](../docs/official_docs/kernel/proc.md).

## Features

| Feature | Default | Pulls in |
|---|---|---|
| `sqlite` | on | `SqliteProcessStore`, `SqliteAgendaStore` |
| `postgres`, `redis` | off | `PostgresProcessStore`, `PostgresAgendaStore`, `RedisProcessStore`, `RedisAgendaStore` |
| `mongodb`, `firestore`, `neo4j` | off | The process, agenda, driver, ledger, partition and cron watermark stores on MongoDB, Firestore or Neo4j, with the same backend of `adk-session`'s stores |
| `webhooks` | on | `webhook_wake_hook` over `adk-server`'s `WebhookIngress`; `WebhookSink`, the outbound webhook sink over `adk-core`'s signed delivery |
| `proc` | on | The activity ledger (`proc`), its stores and `/api/kernel/proc/*`; the OpenTelemetry GenAI histograms of the live process aggregates |
| `proc-baselines` | off | The daily baseline pass and observations over `adk-eval`'s ledger statistics (implies `proc`; on in adk-cli and the umbrella's `full`) |
| `conformance` | off | The `conformance` module |
| `drivers` | on | `drivers`: manifests, the driver store, the supervisor over `adk-tool`'s MCP manager, the stdio launcher on `adk-sandbox`'s enforcers |
| `service` | on | The `service` module; `windows-service` on Windows |
| `managed` | off | `managed`: `KernelDef` under the `adk.kernel` metadata key of a managed definition, and `TenantKernel`, one kernel per tenant over its `ManagedStores` on `adk-managed`'s runtime (Experimental) |
| `channels` | off | The chat channel contract (spec 3.3 §10 K1): `channels`, the binding store on every backend, and with `conformance` the adapter contract suite |

## Tests

| Suite | Covers |
|---|---|
| `tests/kernel_scenarios.rs` | K2: ten sleepers on ten kinds of wake set make no model call and hold no lease over a clock-controlled hour. K3: a file save, a signed webhook and a queue message each wake one run in under 2 s, and redeliveries wake nothing. K9: two replicas over one SQLite file, six running processes stopped within 1 s of a freeze on both (no model or tool call after it) and parked, resumed by the unfreeze with every tool call once. Chaos: a kernel process killed with `SIGKILL` mid-step, restarted, resumes from its checkpoint |
| `tests/supervisor_tests.rs`, `tests/supervisor_property_tests.rs` | The process actions, lifecycle and spend attribution; sleepers never call the model and freezes stop everything within a second, with process-store writes delayed up to 120 ms |
| `tests/wake_*.rs` | Matching, keys, expiry, reconciliation and each wake source |
| `tests/proactivity_scenarios.rs` | K4: in Ask a follow-up waits in the inbox and nothing runs until it is approved, edited or rejected; in Auto it runs after quiet hours within the daily budget, a proposal past `max_proposed_open` is rejected with a reason on the bus, and an over-budget process sleeps until the budget replenishes. K8: three background runs at a quota of three, a typed message runs at once, the youngest background run parks and is requeued, no tool runs twice and the quota is never exceeded. A consolidation item waits for a person in Auto |
| `tests/process_store_*.rs`, `tests/agenda_store*.rs`, `tests/driver_store_*.rs`, `tests/ledger_store_*.rs` | The conformance suites on every backend; PostgreSQL and Redis run `#[ignore]`d against a server (`scripts/test-postgres.sh`) |
| `tests/store_conformance_mongodb.rs`, `tests/store_conformance_firestore.rs`, `tests/store_conformance_neo4j.rs` | The process, agenda, driver, ledger, partition and cron watermark suites on MongoDB, Firestore and Neo4j, `#[ignore]`d; CI's nightly `durable-stores` job runs them against MongoDB (a replica set and a standalone server), Neo4j and the Firestore emulator |
| `tests/driver_scenarios.rs` | K6 on a real stdio MCP server: mount, crash and restart with backoff, a redeclared tool stale and `NeedsReapproval`, re-approval; a `secret://` value reaches the server as a file and never in its environment; what holds the driver's process tree is recorded, and on Windows unmounting ends a process the driver started |
| `tests/driver_park_restart.rs` | An agent run's driver call parked on its server's question survives a kernel restart: answered in the inbox, it completes with the tool executed once; a call approved before its question is not asked about again |
| `tests/driver_sandbox_tests.rs` | A driver under `sandbox = "os"` cannot write outside its allowed paths (Seatbelt, bubblewrap in the `sandbox-linux` job) |
| `tests/driver_*_property_tests.rs` | Any declaration change makes a pinned tool stale; a re-issued round never executes a completed call twice under the action ledger |
| `src/control/tests.rs` | The auth matrix on every transport: socket peers of another uid refused; on Windows the pipe's owner-only descriptor, its owner admitted, and a client of another identity refused by the DACL and, behind an open DACL, by the token check |
| `src/service/tests.rs` | The launchd plist, systemd unit and Windows service rendered for each platform on every platform; `plutil` accepts the plist; the log is created `0600`. Real installs run in `.github/workflows/kernel-service.yml` |
| `tests/managed_def_fixtures.rs`, `tests/managed_tenants.rs`, `tests/managed_tenant_property_tests.rs` (feature `managed`) | The kernel block's golden fixtures and refusals; a definition round-trips through the managed state store; a cron wake for one tenant runs on its stores only; a held call runs once after the tenant's approval; no wake, process or approval crosses tenants |
| `tests/managed_drivers.rs`, `tests/managed_cron_watermarks.rs` (feature `managed`) | A definition's driver mounts on its tenant only when a person approves, and another tenant never sees it; missed cron ticks fire once each as each wake set's policy picks them, across a restart and two replicas; an expression hosted again after it was idle replays nothing from the idle time |
| `tests/sleeper_cron_watermarks.rs` | Cron sleepers over SQLite on a manual clock: a restarted kernel, or two replicas taking over, fire the occurrences missed while no kernel ran once each as `fireOnce`, `catchUp(2)` and `skip` pick them; a second restart replays nothing; a new series replays nothing from before it began |
| `tests/cron_watermark_store_sqlite.rs`, `tests/cron_watermark_store_postgres.rs`, `tests/cron_watermark_store_redis.rs` | The cron watermark store's suite in memory and on SQLite; PostgreSQL and Redis in CI's `postgres` job; MongoDB, Firestore and Neo4j in `tests/store_conformance_*.rs` |
| `tests/k10_baselines.rs` | K10 on a manual clock: six weeks of a routine whose last two end late yield one observation on the tenth day of the change, none in the warm-up or the cooldown, and again after it; each an agenda question that stays open in Auto, starts no run, is remembered once when confirmed and not at all when rejected; none without `proc-baselines` |
| `tests/proc_property_tests.rs` | K-I19: no ledger row carries a payload of the event or audit record that produced it |
| `tests/pairing_scenarios.rs` | Spec 3.3 K2 on loopback HTTP: a `read` device reads but cannot decide, and once revoked is refused; a `stop` device freezes but cannot resume; a device token is never the loopback operator; a device decides medium risk but not high; a code pairs one device; every pairing action is recorded with what it grants; an open event stream ends on revocation and on expiry; a claim or a chat acceptance that cannot be recorded pairs nothing and keeps its code; a client holding a revoked token re-pairs; a waiting chat account is dismissed; a kernel without pairing says what it lacks |
| `tests/channel_gateway.rs` (features `channels`, `conformance`) | The gateway's rules: chat never decides money, a restarted gateway keeps ringing, a decision is carded once, a quorum answer waits for the others, only who may answer gets a card, and concurrent unpaired messages keep the waiting limit |
| `tests/channel_scenarios.rs`, `tests/channel_contract.rs` (feature `channels`) | Spec 3.3 K1 with a scripted agent: a stranger gets a code and starts nothing, once paired its message is answered in its thread; a held call's card reaches the chat without its arguments, "Allow once" runs it once, and a later answer from a paired phone or the chat is already decided. The adapter contract on `InMemoryChannel` |
| `tests/device_store_*.rs`, `tests/channel_store_*.rs` | The device and channel binding suites on every backend |
| `tests/surface_routes.rs` | Spec 3.3 K3–K5 on loopback HTTP: settings start as configured, each change replaces its value, is recorded, survives a restart and holds or releases work at its next admission; "While you were away" counts, spends, says whether quiet time was respected, links each line to its record and moves on only once seen, each device on its own cursor, exactly across a restart; memory and record reach every reader; no device and no caller without `operator` changes a setting or a memory |
| `tests/away_property_tests.rs` | Spec 3.3 K4: whatever rows are written, pages read and marked, and restarts made, the lines a viewer marks seen are every line of the ledger once, in order, with its spend |
| `tests/control_store_*.rs` | The control store suite on every backend |

## License

Apache-2.0
