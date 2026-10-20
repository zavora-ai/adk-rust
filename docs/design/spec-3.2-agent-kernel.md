# Spec 3.2: Agent Kernel — the always-on form of the HarnessAgent

| | |
|---|---|
| **Status** | Draft / RFC |
| **Builds on** | [`spec-3.1-harness-agent.md`](spec-3.1-harness-agent.md): the kernel is the harness's resident form, and `kernel` becomes a default feature of `adk-harness` |
| **Target** | `adk-rust` 3.2.0 (supervisor, process model, wake sources, event bus, control plane, CLI); 3.2.x for drivers with manifests, partitions, `/proc` baselines, service installation, the Windows deliverables and the managed form |
| **Grounding commit** | `origin/main` @ `a24ebd8d` for every `file:line` reference (the only commits since are documentation) |
| **Affected crates** | NEW `adk-kernel`; `adk-harness` (feature `kernel`, default on); `adk-cli` (`adk-rust up`, `attach`, `ps`, …); additive changes in `adk-agent` (ambient), `adk-server` (background, webhooks), `adk-tool` (MCP manager), `adk-devtools`, `adk-sandbox`; `adk-managed` (managed form) |
| **Stability** | `adk-kernel` enters at **Beta** |

---

## 1. Summary

In 2026 the agent products people want are no longer sessions: they keep memory, permissions and task state between runs and keep working while the user is away. The reported Codex "persistent mode" sleeps and re-samples, and its "proactivity" generates follow-up tasks across sessions, with changes outside the user's system still needing approval; OpenClaw's gateway is a daemon that owns sessions, channel routing, tool dispatch, a heartbeat and scheduled tasks, with every client attaching to it; Cloudflare Computer gives each agent its own computer whose file and shell actions are gated and audited; the AIOS and AgentOS papers call the layer that owns scheduling, context, memory, storage and access control an *agent kernel*. None of these is governed, durable and portable at once: the daemon has weak defaults, the hosted computers run only on the vendor's cloud, and the kernel papers are research ([sources](#references)).

ADK-Rust already holds most of a kernel: durable runs that park and resume under fenced leases ([`adk-core/src/run_store.rs`](../../adk-core/src/run_store.rs), [`adk-server/src/background/worker.rs`](../../adk-server/src/background/worker.rs)), a scheduler and schedule store, ambient triggers for cron, file changes and webhooks, signed webhook ingress, queue sources, a replicated kill switch, supervised MCP child processes with restart policies, identity with tenants and classes, budgets, a spend ledger and a signed audit log. What is missing is the one thing that turns those parts into an always-on system: a **resident supervisor with a process model, wake semantics and an event bus**. That is the Agent Kernel.

The kernel is a core capability of the harness, not a separate product. `adk-rust up` starts it; the spec 3.1 harness is the user-space it hosts; Agentrix OS ([§4](#4-relationship-to-spec-31-and-agentrix-os)) becomes a thin application on it.

Three rules shape the design:

| Rule | Consequence |
|---|---|
| **Always resumable, not always running.** An agent that is asleep costs nothing: no model calls, no lease, no CPU. "Always on" means a wake set and durable state, not a hot loop. | Sleep is the `Waiting` state of a durable run with a recorded wake set; the kernel wakes it on time, event, message, approval or completion ([§7.4](#74-sleep-and-wake)). |
| **Every wake is a governed run.** Whatever wakes an agent, the work it does passes the same path as an interactive turn: policy, guardrails, confirmation, ledger, audit. Proactive work is a queue a human can see, bound by budget and quiet hours. | Wake sources produce `NewRun`s, never direct tool calls ([§7.4](#74-sleep-and-wake), [§7.5](#75-proactivity)). |
| **The kernel owns state, surfaces attach.** CLI, web, phone and voice are terminals on one running system; closing a laptop changes nothing. | A control plane over a local socket and HTTP, with the same API for every surface ([§7.11](#711-control-plane-and-surfaces)). |

## 2. Goals

- **G1.** `adk-rust up` keeps a user's agents alive across laptop sleep, process restarts and host reboots, on macOS, Linux and Windows, and on servers, containers and the managed runtime.
- **G2.** An agent that is asleep makes no model calls and holds no lease; wake latency on a local event is under two seconds and on a scheduled time within one scheduler tick.
- **G3.** Every piece of background work is a durable run under the harness's modes, standing rules, budgets and audit; nothing bypasses `authorize_tool_call`.
- **G4.** Proactive work (follow-ups the agent proposes for itself) is visible, approvable, budgeted and bounded by quiet hours before it runs.
- **G5.** Any surface attaches to the running kernel and sees the same process table, agenda, inbox and event stream; an approval from a phone resumes a run started in a terminal.
- **G6.** Integrations are supervised processes with declared capability manifests; a driver that crashes restarts with backoff, and a driver that changes its tool declarations needs re-approval.
- **G7.** Partitions keep areas of a user's life or an organisation's teams isolated by default, with cross-partition work only through the coordinator and under approval.
- **G8.** The kill switch stops every process on every replica within one second and resumes them later without re-executing anything.
- **G9.** The kernel observes itself: a content-free activity ledger, per-user baselines and drift observations phrased in neutral language, with the model phrasing only.

## 3. Non-goals

- **N1.** Not a new runtime, scheduler algorithm or orchestration model. The durable worker, schedule store, ambient triggers, teams and graphs are reused as they are.
- **N2.** Not a hosted service. The kernel runs where the user puts it; the managed runtime remains the multi-tenant hosted form ([§7.13](#713-managed-form)).
- **N3.** Not a chat UI or a mobile app. Surfaces attach through the control plane; the embedded web UI and chat channels are their own roadmap items.
- **N4.** Not an operating system in the hardware sense: no device drivers below MCP and HTTP, no process isolation beyond what `adk-sandbox` and containers provide.
- **N5.** Not information-flow labels. Untrusted content stays confined to the delimited blocks of spec 3.1 §7.7.

## 4. Relationship to spec 3.1 and Agentrix OS

Spec 3.1 defines the agent (`HarnessAgent`), its policy (`HarnessMode` as a `ToolPolicy`), standing approvals, the shell, the team, the plan graph and the goal engine. Spec 3.2 defines the process the agent lives in. Everything that happens inside a run is 3.1; everything about when a run exists, what wakes it, how it is scheduled and how the user reaches it is 3.2. The `kernel` feature of `adk-harness` is on by default, so `cargo install adk-cli` gives both.

Agentrix OS ([`zavora-ai/agentrix-os`](https://github.com/zavora-ai/agentrix-os), `docs/PERSONAL_AI_OS.md`) worked out the operating-system framing for a single person's digital life. Its concepts transfer as follows:

| Agentrix concept | Kernel equivalent | Note |
|---|---|---|
| Mother Agent as kernel and scheduler | The kernel supervisor plus a harness coordinator (3.1 §7.10) | The Mother has no outside-reaching tools; the coordinator inherits that as a partition rule |
| Specialised agents, one per life area | Harness members and partitions' default agents | Members are 3.1 `MemberSpec`s |
| Work World / Home World | Partitions ([§7.8](#78-partitions)) | Isolated memory scopes and policy classes, reconciled only by the coordinator |
| Observe / Suggest / Automate per agent, per tool effect (§10) | `HarnessMode` ReadOnly / Ask / Auto per partition and agent; effect classes are 3.1 `ToolKind` plus driver manifests | Plan mode has no Agentrix counterpart |
| Agent contract `agents/<world>/<agent>.toml` with `[tools]` effect classes (§6.1) | Driver manifest ([§7.7](#77-drivers)) | The declarative half of an integration |
| Activity ledger, content-free (§7.1) and personal baseline (§8) | `/proc` self-observation ([§7.10](#710-proc-self-observation)) | Median/MAD, 14-day warm-up, cooldowns, neutral language |
| Recipes ("when X, do Y") (§2.5) | Standing rules (3.1 §7.5) plus plan graphs (3.1 §7.11) on a wake set | A recipe is an approved automation bound to a trigger |
| Memory items `known` / `assumed` / `recommended` with provenance (§11) | Memory with provenance in 3.1 §7.15 and the 3.x memory work | Proactive consolidation writes only `assumed` |
| Pending actions, approvals inbox, audit, undo | 3.1 approvals, the approvals inbox, the signed audit log | Undo is a 3.2.x item |
| Ten design principles (§1.3) | Carried into [§9](#9-security-invariants-and-audit-records): least privilege, human in control, honest data, memory is the user's, health and money stay conservative | |

What does not transfer: the Mother Agent as a special class of agent (it is a harness in a coordinator role), and the spatial UI (a surface).

## 5. Headline capabilities and demonstration scenarios

Each scenario is a test in `examples/harness_kernel` ([§11](#11-testing-and-live-verification)); the pass condition is asserted on stores, processes, timings and audit records.

| # | Capability | Scenario | Pass condition | Machinery |
|---|---|---|---|---|
| K1 | **Leave it running.** The kernel survives sleep, restarts and reboots and keeps its promises | `adk-rust up --install`; schedule a nightly goal; reboot the host; next morning | The goal ran once at its time; the process table shows it `Completed`; the audit log has one iteration chain | service install ([§7.12](#712-service-installation)), `Scheduler`, `ScheduleStore`, `DurableWorker` |
| K2 | **Sleep is free.** Asleep means no model calls, no lease, no CPU | Put ten agents to sleep on wake sets; wait an hour | Zero model calls, zero spend, zero claimed leases; `ps` shows ten `Waiting` with their wake sets | `RunState::Waiting`, `WakeSet` ([§7.4](#74-sleep-and-wake)) |
| K3 | **It wakes on what matters.** Time, a file change, a signed webhook, a channel message, an approval, a finished run | Save `Cargo.lock`; a run wakes within 2 s and audits the repo; a webhook wakes another | Each wake creates one governed run with the wake reason in its record; duplicate deliveries wake nothing | `EventSource`, `FileWatchTrigger`, `WebhookIngress`, idempotency keys |
| K4 | **It proposes its own work, under control.** Follow-ups the agent proposes queue for approval, bounded by budget and quiet hours | The agent proposes "re-run the tests after the dependency bump lands" | The proposal appears in the agenda as `Proposed`; in Ask mode nothing runs until approved; in Auto it runs after quiet hours within the daily budget | agenda, `ProactivityPolicy` ([§7.5](#75-proactivity)) |
| K5 | **One job, many windows.** Every surface attaches to the same running state | Start a goal in the terminal; close the laptop lid; approve the held call from a phone over the control plane | The run resumes without the terminal; `attach` from a new shell shows the same process and transcript | control plane ([§7.11](#711-control-plane-and-surfaces)), parked approvals |
| K6 | **Drivers you can trust.** Integrations are supervised processes with manifests | Mount an MCP driver; kill it; redeclare one of its tools | It restarts with backoff; the changed tool is marked stale and its standing rules stop matching until re-approved | `McpServerManager`, driver manifests ([§7.7](#77-drivers)), 3.1 digest binding |
| K7 | **Partitions.** Work and home never see each other's data | A work agent asks for a home calendar | Denied at the policy; the coordinator can ask, under approval, and the audit shows the crossing | partitions ([§7.8](#78-partitions)) |
| K8 | **Attention.** A human message preempts background work; quotas hold | Start three background runs at the per-partition quota; type a message | The message's run starts within one tick; one background run is parked and requeued; the quota is never exceeded | priority queues, preemption ([§7.9](#79-scheduling-attention-and-resources)) |
| K9 | **One switch stops everything.** Freeze across replicas | Two kernel replicas, six running processes; `freeze` | Every process stops within 1 s on both replicas; `unfreeze` resumes them; the ledger shows no tool executed twice | `GovernanceControl::mirrored`, action ledger |
| K10 | **It notices drift without judging.** Content-free ledger and baselines | Two weeks of late finishes | One observation in neutral language, offered as a question, after the warm-up and cooldown rules | `/proc` ([§7.10](#710-proc-self-observation)) |

K1, K2, K8 and K9 together — durable, free when asleep, preemptible, freezable everywhere — have no equivalent in any shipping agent product.

## 6. Architecture

```text
┌──────────────────────────────────────────────────────────────────────────────────┐
│ SURFACES   adk-rust attach · web UI · phone · voice · channels · adk-rust ps/kill  │ §7.11
│            all over the control plane (local socket · HTTP) — none holds state     │
├──────────────────────────────────────────────────────────────────────────────────┤
│ KERNEL (adk-kernel)                                                                │
│   Supervisor ─ Scheduler ─ WakeSources(cron · files · webhooks · queues · channels) │ §7.2–7.4
│   Process table (runs) ─ Agenda & proactivity ─ Event bus ─ /proc                  │ §7.3, 7.5, 7.6, 7.10
│   Drivers (supervised MCP processes with manifests) ─ Partitions ─ Quotas           │ §7.7–7.9
├──────────────────────────────────────────────────────────────────────────────────┤
│ USER SPACE  HarnessAgent (spec 3.1): modes · standing rules · shell · team · graph │
├──────────────────────────────────────────────────────────────────────────────────┤
│ GOVERNANCE  authorize_tool_call · kill switch (mirrored) · budgets · spend · audit  │
├──────────────────────────────────────────────────────────────────────────────────┤
│ DURABILITY  DurableRunStore · leases · ScheduleStore · ApprovalStore · sessions     │
└──────────────────────────────────────────────────────────────────────────────────┘
```

**Decisions**

| | Decision | Grounding |
|---|---|---|
| **D-1** | The kernel is the harness's resident form: one `adk-kernel` crate for the runtime pieces, a default-on `kernel` feature in `adk-harness`, `adk-rust up` as the command. | A separate crate because the kernel composes `adk-server`'s background machinery, `adk-agent`'s ambient triggers and `adk-tool`'s MCP manager, which `adk-harness` should not depend on wholesale |
| **D-2** | A process is a durable run. The process table is a view over `DurableRunStore` plus a kernel-owned side table. | `RunRecord` has no metadata field (3.1 design notes); a side table avoids a schema change across six backends |
| **D-3** | Every wake produces a `NewRun` (or wakes a parked run); no wake source calls a tool directly. | `WebhookIngress` and the scheduler already enqueue `NewRun`s ([`ingress.rs`](../../adk-server/src/webhooks/ingress.rs), [`scheduler.rs:152`](../../adk-server/src/background/scheduler.rs)) |
| **D-4** | Asleep is `Waiting` with a recorded wake set and no lease. | `RunState::Waiting` and `DurableRunStore::wait`/`wake` ([`run_store.rs:88, 620`](../../adk-core/src/run_store.rs)) |
| **D-5** | Priority is a queue. Workers are configured per queue; `nice` moves a run between queues. | `NewRun.queue` and `DurableWorkerConfig.queue` exist ([`run_store.rs:189`](../../adk-core/src/run_store.rs), [`worker.rs:126`](../../adk-server/src/background/worker.rs)); `claim` has no priority argument |
| **D-6** | Drivers are MCP child processes supervised by `McpServerManager`, described by a manifest that extends `McpServerConfig`. | [`adk-tool/src/mcp/manager/manager.rs:61-1224`](../../adk-tool/src/mcp/manager/manager.rs), [`config.rs:63-143`](../../adk-tool/src/mcp/manager/config.rs) |
| **D-7** | Surfaces attach over a control plane and hold no state; the same API serves the CLI, the web UI and phones. | OpenClaw's gateway shape, with authentication and audit |
| **D-8** | Cross-platform by construction, with Windows gaps named as deliverables, not caveats. | [§7.12](#712-service-installation), [§10](#10-changes-outside-the-new-crate) W1–W5 |

## 7. Component specifications

### 7.1 Crate layout and features

```text
adk-kernel/src/
  lib.rs            re-exports, feature gates
  kernel.rs         Kernel, KernelBuilder, KernelConfig, lifecycle (start/stop/pause/drain), health
  process.rs        Process, ProcessTable (view over DurableRunStore + ProcessStore), ps/kill/nice/suspend/resume
  wake.rs           WakeSet, WakeSource trait, adapters: cron, file, webhook, queue, approval, run-finished, channel
  agenda.rs         Agenda, AgendaItem, ProactivityPolicy, the consolidation tick
  bus.rs            KernelEvent, EventBus, EventSink (SSE, channels, webhooks out, audit)
  drivers.rs        DriverManifest, DriverStore, DriverSupervisor over McpServerManager
  partition.rs      Partition, PartitionStore, quotas
  sched.rs          priority queues, preemption, quotas, quiet hours, cost-aware admission
  proc.rs           activity ledger, baselines, observations
  control/          control plane: API types, local-socket transport, HTTP router, auth
  service/          launchd · systemd · Windows service installation            [service]
  stores/           in-memory · file · sqlite backends for the kernel-owned stores [sqlite]
```

| Feature | Default | Pulls in |
|---|---|---|
| `sqlite` | on | SQLite backends for `ProcessStore`, `AgendaStore`, `DriverStore`, `PartitionStore`, `LedgerStore` through `adk-session` |
| `webhooks` | on | `adk-server/webhook-ingress` |
| `queues` | off | queue adapters beyond the in-memory source |
| `channels` | off | channel wake sources (Telegram, Slack) once the channel work lands |
| `drivers` | on | `adk-tool` MCP manager |
| `service` | on | service installation per platform (`windows-service` on Windows) |
| `proc` | on | ledger and baselines |
| `managed` | off | `adk-managed` bridge ([§7.13](#713-managed-form)) |

Dependencies: `adk-core`, `adk-runner`, `adk-session`, `adk-agent` (ambient), `adk-server` (background, webhooks, approvals), `adk-tool` (MCP manager), `adk-auth` (signed audit); `notify` 8 already used by the file-watch trigger ([`adk-agent/Cargo.toml:41`](../../adk-agent/Cargo.toml)). `adk-harness` gets `kernel = ["dep:adk-kernel"]`, on by default, exposing `Harness::kernel()` and the resident mode of `HarnessLauncher`.

### 7.2 Supervisor

```rust
pub struct Kernel { /* private */ }

pub struct KernelConfig {
    pub app_name: String,                 // "adk-rust"
    pub home: PathBuf,                    // dirs::config_dir()/adk-rust (stores, socket, logs)
    pub queues: Vec<QueueClass>,          // see §7.9; default: interactive, background, maintenance
    pub worker: DurableWorkerConfig,      // adk-server/src/background/worker.rs:122 (per queue)
    pub scheduler_tick: Duration,         // 1 s
    pub proactivity: ProactivityPolicy,   // §7.5
    pub quiet_hours: Option<QuietHours>,  // local time window, per partition override
    pub governance: GovernanceControl,    // GovernanceControl::mirrored(..) on multi-replica
    pub control: ControlPlaneConfig,      // socket path / named pipe, HTTP bind, extractor
    pub fail_if_sandbox_unavailable: bool,
}

impl Kernel {
    pub fn builder() -> KernelBuilder;
    pub async fn start(&self) -> Result<()>;          // workers, scheduler, wake sources, drivers, control plane
    pub async fn pause(&self) -> Result<()>;          // stop claiming; running processes continue to their next checkpoint
    pub async fn resume(&self) -> Result<()>;
    pub async fn drain(&self, timeout: Duration) -> Result<()>;   // no new claims; wait for running processes to park
    pub async fn stop(&self) -> Result<()>;           // drain, stop drivers, close the control plane
    pub fn processes(&self) -> &ProcessTable;
    pub fn agenda(&self) -> &Agenda;
    pub fn events(&self) -> EventSubscriber;
    pub fn health(&self) -> KernelHealth;             // per worker, scheduler lag, driver states, store latency
}
```

Composition, all existing APIs:

| Kernel part | Built from |
|---|---|
| Workers | One `DurableWorker` per queue class, each with `DurableWorkerConfig { queue, lease_ttl, poll_interval, max_concurrent_runs, run_timeout, approval_timeout, allowed_principals }` ([`worker.rs:122-145`](../../adk-server/src/background/worker.rs)); targets registered through `BackgroundState::with_agent(target, Arc<Runner>)` ([`background/mod.rs:558`](../../adk-server/src/background/mod.rs)) and `with_executor` (269, 551) for graph goals |
| Scheduler | `Scheduler::new(schedules, runs)` and `tick()` ([`scheduler.rs:99, 152`](../../adk-server/src/background/scheduler.rs)) over a `ScheduleStore` ([`adk-core/src/schedule.rs:450`](../../adk-core/src/schedule.rs): `upsert`, `get`, `list`, `delete`, `set_paused`, `due`, `advance`); `Schedule { id, spec, run: NewRun, missed: MissedFirePolicy, paused, next_fire_at, revision }` (132); `ConcurrencyPolicy { Skip, Allow, Queue }` ([`cron.rs:49`](../../adk-server/src/background/cron.rs)) |
| Wake sources | [§7.4](#74-sleep-and-wake) |
| Drivers | `McpServerManager::new(configs)`, `start_all`, `start_monitoring`, `restart_server`, `shutdown` ([`manager.rs:290-1224`](../../adk-tool/src/mcp/manager/manager.rs)) |
| Approvals | `StoreApprovalService::with_run_store` ([`adk-core/src/approval.rs:1470, 1522`](../../adk-core/src/approval.rs)), so a decision wakes the parked run, plus `spawn_approval_ticker` for expiries ([`adk-server/src/approvals.rs:163`](../../adk-server/src/approvals.rs)) |
| Kill switch | `GovernanceControl::mirrored(store, GovernanceMirrorConfig { poll_interval, max_staleness, fail_open, scopes, watch })` ([`governance.rs:182-194, 327`](../../adk-core/src/governance.rs)); `freeze` (350), `unfreeze` (361) |
| Control plane | [§7.11](#711-control-plane-and-surfaces) |

Lifecycle semantics: `pause` stops claiming new runs and lets running processes reach their next checkpoint (a model-call boundary or a parked approval); `drain` waits for that and then releases leases; `stop` additionally stops drivers and closes the socket. A `SIGTERM` is `drain(30 s)` then `stop`. Nothing is lost on a hard kill: leases expire and another claim resumes from the checkpoint ([`worker.rs:789-979`](../../adk-server/src/background/worker.rs)).

### 7.3 Process model

```rust
pub struct Process {
    pub run: RunRecord,                    // adk-core/src/run_store.rs:360-428
    pub meta: ProcessMeta,                 // kernel-owned side table, keyed by run id
}
pub struct ProcessMeta {
    pub pid: String,                       // = run id
    pub parent: Option<String>,            // run id that spawned it (goal iteration, proposal, trigger)
    pub partition: PartitionId,
    pub queue: QueueClass,                 // priority class (§7.9)
    pub kind: ProcessKind,                 // Interactive | Goal | Scheduled | Triggered { source } | Proposed | Maintenance
    pub wake: Option<WakeSet>,             // while Waiting
    pub agenda_item: Option<String>,
    pub labels: BTreeMap<String, String>,
}
#[async_trait]
pub trait ProcessStore: Send + Sync {      // in-memory, file, sqlite
    async fn put(&self, meta: ProcessMeta) -> Result<()>;
    async fn get(&self, pid: &str) -> Result<Option<ProcessMeta>>;
    async fn list(&self, filter: &ProcessFilter) -> Result<Vec<ProcessMeta>>;
    async fn set_wake(&self, pid: &str, wake: Option<WakeSet>) -> Result<()>;
    async fn set_queue(&self, pid: &str, queue: QueueClass) -> Result<()>;
}
impl ProcessTable {
    pub async fn ps(&self, filter: ProcessFilter) -> Result<Vec<Process>>;
    pub async fn kill(&self, pid: &str, reason: &str) -> Result<()>;        // DurableRunStore::request_cancel
    pub async fn nice(&self, pid: &str, queue: QueueClass) -> Result<()>;   // requeue a Queued/Waiting run
    pub async fn suspend(&self, pid: &str) -> Result<()>;                   // interrupt at the next checkpoint, then Waiting { Manual }
    pub async fn resume(&self, pid: &str) -> Result<()>;                    // DurableRunStore::wake
}
```

| OS notion | Kernel mapping |
|---|---|
| Process states | `RunState::{Queued, Running, Waiting, Completed, Failed, Cancelled}` ([`run_store.rs:88`](../../adk-core/src/run_store.rs)); `Waiting` is sleep |
| Process tree | `parent` in `ProcessMeta`; a goal's iterations, a plan graph's steps and proposed follow-ups are children |
| Time slice | The lease; a process yields at every model-call boundary, where the worker heartbeats and checks for cancel and freeze |
| `kill -TERM` / `kill -KILL` | `request_cancel` (cooperative, at the next checkpoint) / lease revocation plus `Runner::interrupt` |
| Zombie | A `Running` run whose lease expired; the next claim resumes it from its checkpoint |

Suspending a running process uses `Runner::interrupt` for the in-flight turn and records a `Session` checkpoint; the resumed run continues from the session, and the action ledger guarantees no completed tool call runs twice (3.1 §7.12).

### 7.4 Sleep and wake

```rust
pub enum Wake {
    At(DateTime<Utc>),                          // not_before on the run
    Cron { expr: String, missed: MissedFirePolicy },   // adk-core/src/schedule.rs:97
    FileChange { root: PathBuf, glob: String, debounce: Duration },
    Webhook { hook: String },                   // adk-server/src/webhooks/ingress.rs:94 WebhookHook
    Queue { source: String },                   // adk-core/src/queue_source.rs:112
    Channel { channel: String, thread: Option<String> },   // [channels]
    Approval { fingerprint: String },           // RunCheckpointRef::AwaitingApproval
    RunFinished { pid: String },
    BudgetReplenished { partition: PartitionId },
    Manual,
}
pub struct WakeSet { pub any_of: Vec<Wake>, pub expires_at: Option<DateTime<Utc>>, pub on_expiry: ExpiryAction /* Cancel | Run */ }

#[async_trait]
pub trait WakeSource: Send + Sync {   // adapter over adk_agent::ambient::EventSource where one exists
    fn name(&self) -> &str;
    async fn subscribe(&self) -> Result<BoxStream<'static, WakeEvent>>;
}
pub struct WakeEvent { pub source: String, pub key: String /* idempotency */, pub payload: Value, pub principal: Option<String>, pub at: DateTime<Utc> }
```

How a wake becomes work:

1. A `WakeEvent` arrives from a source. Cron and file changes come from `adk-agent`'s ambient triggers: `CronTrigger::new(expr).with_missed_tick_policy(..).with_watermark(..).with_max_catch_up(..)` ([`cron_trigger.rs:73-127`](../../adk-agent/src/ambient/cron_trigger.rs)) and `FileWatchTrigger::new(path, pattern)` ([`file_watch_trigger.rs:22-36`](../../adk-agent/src/ambient/file_watch_trigger.rs)), both `EventSource`s (`name`, `subscribe() -> BoxStream<TriggerEvent { source, payload, principal }>`, [`event_source.rs:11-36`](../../adk-agent/src/ambient/event_source.rs)). Webhooks come from `WebhookIngress::new(store).with_hook(..).with_governance(..)` ([`ingress.rs:344-379`](../../adk-server/src/webhooks/ingress.rs)), which verifies the signature and enqueues idempotently. Queues come from `QueueSource::{pull, ack, nack}` ([`queue_source.rs:112`](../../adk-core/src/queue_source.rs)).
2. The kernel matches the event against the wake sets of `Waiting` processes and against standing schedules and triggers.
3. For a `Waiting` process it calls `DurableRunStore::wake(run_id)` and records the wake reason; for a trigger with no sleeping process it enqueues a `NewRun` with `idempotency_key = "<source>:<key>"`, `concurrency_key = <session or partition>`, the partition's `queue`, and the principal as a `Service` subject (3.1 §7.14). A duplicate delivery hits the idempotency key and wakes nothing.
4. The run executes under the harness's `configure_run`, so modes, standing rules, budgets and audit apply exactly as for an interactive turn.

Sleep costs nothing: a `Waiting` process holds no lease, no worker polls it, and no model call is made until a wake. Missed wakes while the kernel was down follow `MissedFirePolicy` for schedules and `MissedTickPolicy` with watermarks for cron triggers ([`cron_trigger.rs:24`](../../adk-agent/src/ambient/cron_trigger.rs), [`watermark.rs:62`](../../adk-agent/src/ambient/watermark.rs)); file changes missed while down are reconciled by a scan on start when the wake set asks for it. Latency targets: local event to `Running` under 2 s; scheduled time within one scheduler tick (1 s default).

### 7.5 Proactivity

The agenda is the kernel-scope todo list: items that outlive a session and may become processes.

```rust
pub struct AgendaItem {
    pub id: String, pub partition: PartitionId, pub subject: SubjectRef,
    pub title: String, pub rationale: String,
    pub origin: AgendaOrigin,             // User | Agent { pid, confidence } | Consolidation | Observation { id }
    pub state: AgendaState,               // Proposed | Approved | Scheduled { wake: WakeSet } | Running { pid } | Done | Rejected | Expired
    pub plan: Option<PlanRef>,            // a 3.1 plan graph when the agent produced steps
    pub budget: Option<RunBudget>,        // adk-core/src/budget.rs:105
    pub created_at: DateTime<Utc>, pub expires_at: Option<DateTime<Utc>>,
}
pub struct ProactivityPolicy {
    pub max_proposed_open: usize,         // 10 per partition
    pub daily_budget: RunBudget,          // model calls, tokens, cost for proactive runs per partition
    pub quiet_hours: Option<QuietHours>,  // proactive runs never start inside them
    pub approval: ProactiveApproval,      // FollowMode (default) | AlwaysAsk | Never
    pub consolidation: Option<ConsolidationTick>,   // interval, model ref, memory write policy
}
```

- The harness gains one tool, `propose_followup { title, rationale, when?: Wake, steps?: [PlanStep] }`, class `Todo` in 3.1 terms (allowed in every mode), whose only effect is an `AgendaItem` in state `Proposed`. Proposing is free; running is governed.
- `ProactiveApproval::FollowMode` maps to the partition's `HarnessMode`: in ReadOnly and Plan a proposal stays `Proposed`; in Ask it needs an approval from the inbox (same `ApprovalService`, kind `agenda`); in Auto the kernel schedules it. Agentrix's Observe / Suggest / Automate is this mapping.
- A scheduled item becomes a process when its wake fires, under the daily budget (the kernel admits it only if the partition's proactive `SpendLedger` total leaves room) and outside quiet hours; it carries `ProcessKind::Proposed` and its parent is the proposing run.
- **Consolidation tick** (Letta's sleep-time agents, [sources](#references)): a `Maintenance` process on a cheaper model runs every N completed interactive runs or on a schedule, reads the recent sessions of its partition, and writes memory proposals marked `assumed` and agenda items marked `Proposed`. It has no tools other than memory and agenda, so it cannot act in the world.
- Everything proactive is visible: `adk-rust agenda` lists items; the inbox shows those awaiting approval; the event bus carries every state change; the audit records the proposer.

### 7.6 Event bus

```rust
pub enum KernelEvent {
    Process { pid: String, from: RunState, to: RunState, reason: Option<String> },
    Wake { pid: Option<String>, source: String, key: String },
    Held { pid: String, request: ToolConfirmationRequest },
    Decided { pid: String, fingerprint: String, decision: ToolConfirmationDecision, by: SubjectRef },
    RuleHit { pid: String, rule_id: String },
    Spend { partition: PartitionId, key: SpendKey, delta: SpendDelta },
    Driver { id: String, state: DriverState, reason: Option<String> },
    Agenda { item: String, from: AgendaState, to: AgendaState },
    Observation { id: String, partition: PartitionId },
    Governance { frozen: bool, reason: Option<String> },
    Health(KernelHealth),
}
#[async_trait]
pub trait EventSink: Send + Sync { async fn emit(&self, event: &KernelEvent) -> Result<()>; }
```

Sinks: the control plane's SSE stream (every surface), channel notifiers (`ApprovalNotifier` generalised, [`approval.rs:1199`](../../adk-core/src/approval.rs)), outbound webhooks (Standard Webhooks signatures, mirroring the ingress), and the signed audit log for the events that are audit records ([§9.3](#93-audit-records)). The bus is bounded per sink; a slow sink drops `Health` first and never blocks a process. Events are replayable from the run store and the kernel stores, so a reconnecting surface catches up by cursor rather than by holding a subscription open.

### 7.7 Drivers

A driver is an MCP server run as a supervised child process with a declared manifest. The manifest is Agentrix's agent contract and tool allowlist moved into the kernel:

```toml
# ~/.adk/drivers/email.toml  (or .adk/drivers/ in a workspace; project scope can only restrict)
id          = "email"
command     = "mcp-email"
args        = ["--imap"]
env         = { EMAIL_TOKEN = "secret://email/token" }   # resolved through the credential broker, never inline
partitions  = ["work"]
lifecycle   = "lazy"                       # McpLifecycleMode: lazy | eager
sandbox     = "os"                         # os | container | unsandboxed (recorded)
restart     = { max_attempts = 5, initial_delay_ms = 500, max_delay_ms = 30000, backoff_multiplier = 2.0 }

[tools]                                    # effect class per tool; unknown tools are `other`
list_inbox   = "read"
get_email    = "read"
create_draft = "write_local"
send_draft   = "send_external"
```

```rust
pub struct DriverManifest { pub id: String, pub server: McpServerConfig /* adk-tool/src/mcp/manager/config.rs:63 */,
    pub partitions: Vec<PartitionId>, pub sandbox: DriverSandbox, pub tools: BTreeMap<String, EffectClass>,
    pub data_classes: Vec<String>, pub declaration_digests: BTreeMap<String, String> /* pinned at approval */ }
pub enum EffectClass { Read, WriteLocal, ScheduleWithOthers, SendExternal, PublishPublic, Financial, Delete, Other }
```

- Supervision is `McpServerManager` ([`manager.rs:290-1224`](../../adk-tool/src/mcp/manager/manager.rs)): `add_server`, `start_server`, `restart_server` with `RestartPolicy { initial_delay_ms, max_delay_ms, backoff_multiplier, max_restart_attempts }` ([`config.rs:128-143`](../../adk-tool/src/mcp/manager/config.rs)), `start_monitoring`, `shutdown`. The kernel subscribes to its lifecycle and emits `KernelEvent::Driver`.
- Mounting a driver is an approval: the user sees the command line, the partitions and the effect classes; the kernel pins each tool's declaration digest (3.1 `ToolOrigin`, [`context.rs:1089-1119`](../../adk-core/src/context.rs)). A tool whose declaration later differs is marked `stale`: its standing rules stop matching (3.1 I8) and the driver's `KernelEvent::Driver { state: NeedsReapproval }` goes out.
- Effect classes feed the harness policy: `EffectClass` maps onto 3.1 `ToolKind::External { effect, trusted_origin }` with the manifest's class as the source of truth, so a driver's self-declared `readOnlyHint` never grants anything (3.1 I12). `PublishPublic` and `Financial` are never auto-allowed (Agentrix §10.2; 3.1 I38).
- Drivers run under the 3.1 stdio sandbox policy (a sandboxed launcher where an enforcer exists, `Unsandboxed { reason }` recorded otherwise) and inherit the partition's secrets through the credential broker, never through the environment.

### 7.8 Partitions

```rust
pub struct Partition {
    pub id: PartitionId,                    // "work", "home", "<team>", …
    pub class: String,                      // AgentPrincipal.class for every agent in it
    pub tenant: Option<String>,             // AgentPrincipal.tenant on multi-tenant hosts
    pub default_mode: HarnessMode,          // spec 3.1 §7.4
    pub memory_scope: String,               // project id for project-scoped memory
    pub drivers: Vec<String>,
    pub quotas: Quotas,                     // §7.9
    pub quiet_hours: Option<QuietHours>,
    pub cross_partition: CrossPartition,    // Deny (default) | CoordinatorWithApproval
}
```

A partition is a policy class plus a memory scope plus a driver set. Agents in a partition get its `class` on their `AgentPrincipal` ([`principal.rs:245`](../../adk-core/src/principal.rs)), so `PolicyRule::for_class` rules apply ([`policy.rs:547`](../../adk-core/src/policy.rs)); memory uses project scoping (`MemoryService::{supports_project_scoping, add_session_to_project, add_entry_to_project, delete_project}`, [`adk-memory/src/service.rs:115-175`](../../adk-memory/src/service.rs); read side `adk_core::Memory::search_in_project`, [`context.rs:755`](../../adk-core/src/context.rs)); drivers are mounted per partition. A request that crosses partitions is denied at the policy unless the coordinator makes it, and then it is `RequireApproval` and non-waivable, with the crossing in the audit record. Agentrix's Work and Home worlds are two partitions with `cross_partition: CoordinatorWithApproval`.

### 7.9 Scheduling, attention and resources

| Mechanism | Design | Grounding |
|---|---|---|
| Priority classes | Three queues by default, `interactive`, `background`, `maintenance`, each with its own `DurableWorker` and `max_concurrent_runs`; a human turn is always `interactive` | `NewRun.queue`, `DurableWorkerConfig { queue, max_concurrent_runs }` |
| Preemption | When an `interactive` run cannot start because the partition is at quota, the kernel suspends the youngest `background` run at its next checkpoint (`Runner::interrupt`, then `Waiting { RunFinished(interactive pid) }`) and requeues it | `Runner::interrupt` ([`runner.rs:2178`](../../adk-runner/src/runner.rs)), `request_cancel`, `wake` |
| Quotas | Per partition: max running, max queued, max proposed, daily `RunBudget` for proactive work, spend cap per day; enforced at admission | `RunBudget`, `SpendLedger` ([`spend.rs:376`](../../adk-core/src/spend.rs)), `concurrency_key` |
| Quiet hours | Proactive and maintenance processes never start inside the window; interactive and approval-woken runs always may | `not_before` on the run |
| Cost-aware admission | A background run is admitted only if the partition's remaining daily spend covers its budget's cost cap; otherwise it waits for `BudgetReplenished` | `SpendLedger::spent(key, SpendPeriod::Day)` ([`spend.rs:40, 376`](../../adk-core/src/spend.rs)) |
| Fairness | Round-robin across partitions within a queue; a partition cannot starve another | kernel claim ordering within the worker's batch |

### 7.10 `/proc`: self-observation

The kernel keeps a content-free activity ledger and learns a per-user baseline (Agentrix §7–8):

| Element | Design |
|---|---|
| Ledger | One append-only table: `ts, partition, agent, kind (intent, tool_call, approval, rejection, observation, wake, proposal, …), effect, duration_ms, subject_hash (HMAC, no content), meta (small, schema-checked), trace_id`. Written by the kernel from `KernelEvent`s and by the harness from tool events. Never a payload, never a secret |
| Baseline | Per dimension and day class (weekday/weekend): rolling 28-day median and MAD; 14-day warm-up before any observation; comparison over the last 7 and 14 days; deviation when `|recent − baseline| > max(k·MAD, floor)` with k = 2.5 and per-dimension floors; "meaningful" means two dimensions or one for ten days; a 7-day cooldown per dimension |
| Observations | Offered as questions in neutral language ("Your routine has changed over the last two weeks… would you like to review what changed?"); never a diagnosis; confirmed values become `known` memory, others stay `assumed` |
| API | `GET /proc/ledger`, `/proc/baselines`, `/proc/observations`; `adk-rust proc` |
| Relation to 3.1 §7.13 | The harness's spans, spend and loop guard feed the ledger; the eval suite consumes it |

The model is used only to phrase an observation already detected deterministically. The ledger never leaves the host unless the user exports it.

### 7.11 Control plane and surfaces

One API, three transports: a Unix domain socket (macOS, Linux) or a named pipe (Windows) at the kernel home, and the HTTP server on loopback or, with authentication, on the network. Surfaces hold no state.

| Endpoint | Purpose |
|---|---|
| `GET /kernel/health`, `/kernel/processes`, `POST /kernel/processes/{pid}/{kill,nice,suspend,resume}` | the process table |
| `GET /kernel/agenda`, `POST /kernel/agenda/{id}/{approve,reject,schedule}` | proactivity |
| `GET /kernel/events` (SSE, cursor) | the event bus |
| `GET /kernel/drivers`, `POST /kernel/drivers/{id}/{mount,unmount,restart,reapprove}` | drivers |
| `GET /kernel/partitions`, `GET /kernel/proc/*` | partitions, self-observation |
| `POST /api/run/…`, `/api/approvals`, `/api/runs`, `/api/cron`, `/hooks/{name}` | the existing server routes, unchanged ([`adk-server/src/rest/mod.rs`](../../adk-server/src/rest/mod.rs), [`approvals.rs`](../../adk-server/src/approvals.rs), [`background/mod.rs`](../../adk-server/src/background/mod.rs), [`webhooks/ingress.rs:554`](../../adk-server/src/webhooks/ingress.rs)) |
| `POST /kernel/attach/{session}` | a surface attaches to a session: transcript cursor, pending holds, process |

Authentication follows 3.1 §8.2: the local socket trusts the OS user; HTTP on loopback uses the `LoopbackOperatorExtractor`; anything else requires a bearer token or SSO extractor; every control action is audited with the subject.

`adk-rust attach [--session <id>]` opens the REPL against a running kernel instead of an in-process `Runner`; `adk-rust` with no subcommand uses the kernel when one is running and starts an ephemeral in-process one otherwise, so the 3.1 experience is unchanged.

### 7.12 Service installation

`adk-rust up --install` writes a launchd agent plist (macOS), a systemd user unit (Linux) or registers a Windows Service, each running `adk-rust up` with the kernel home, logging to the home, and restarting on failure; `--uninstall` reverses it. The kernel is a plain foreground process otherwise, which is what containers and the managed runtime use.

### 7.13 Managed form

`adk-managed` already has a supervised session loop that claims, heartbeats, parks and resumes runs across instances over shared stores ([`session_loop.rs:613, 661`](../../adk-managed/src/session_loop.rs)) and a declarative `ManagedAgentDef { name, model, system, description, tools, mcp_servers, skills, permission_policy, metadata }` ([`agent_def.rs:36-61`](../../adk-managed/src/types/agent_def.rs)). The rule between the two: the **kernel hosts a user's or a team's agents on their own infrastructure**; the **managed runtime hosts tenants**. They share the durable stores and the harness, and a `KernelDef` extension of `ManagedAgentDef` (partitions, drivers, proactivity policy, wake sets) lets the managed runtime run a kernel per tenant. `adk-managed` is Experimental, so this follows that tier.

## 8. Surfaces

### 8.1 adk-cli

| Command | Behaviour |
|---|---|
| `adk-rust up [--install\|--uninstall] [--home DIR] [--bind HOST:PORT] [--auth-token]` | Start the kernel (foreground) or install it as a service |
| `adk-rust attach [--session ID]` | REPL on the running kernel; falls back to an ephemeral in-process kernel when none runs |
| `adk-rust ps [--partition P] [--state S]`, `kill PID`, `nice PID QUEUE`, `suspend PID`, `resume PID` | Process table |
| `adk-rust agenda [list\|approve ID\|reject ID\|schedule ID --at\|--cron]` | Proactivity |
| `adk-rust wake --file GLOB\|--cron EXPR\|--webhook NAME -- "task"` | Create a sleeping process with a wake set |
| `adk-rust drivers [list\|mount FILE\|unmount ID\|restart ID\|reapprove ID]` | Drivers |
| `adk-rust partitions [list\|add\|set-mode P MODE]` | Partitions |
| `adk-rust proc [ledger\|baselines\|observations]` | Self-observation |
| `adk-rust freeze [--reason]` / `unfreeze` | Kill switch across replicas |
| `adk-rust events [--follow]` | The event bus |

`goal` and `serve` from spec 3.1 run on the kernel when one is present: a goal is a scheduled or triggered process, `serve` is the kernel's HTTP control plane with the harness mounted.

### 8.2 adk-server, template, umbrella

The control plane is an Axum router merged into `ServerBuilder` with `add_api_routes` so the existing auth middleware applies. The `harness` template gains a `[kernel]` section in `.adk/harness.toml` (partitions, drivers directory, proactivity policy, quiet hours) and `cargo run -- up`. The umbrella gets `kernel = ["harness", "dep:adk-kernel"]` in `full`, with the module `adk_rust::kernel`.

## 9. Security invariants and audit records

### 9.1 Threats specific to always-on

| Threat | Mechanism in the design |
|---|---|
| A bounded session's risks become permanent: an injection in one run poisons every later run through memory or agenda | Memory writes from proactive runs are `assumed` and need approval before they become `known` (3.1 I33); agenda proposals are `Proposed` until a human or the mode admits them; the consolidation tick has no world-reaching tools |
| Runaway proactivity: the agent keeps generating work for itself | `max_proposed_open`, daily proactive `RunBudget`, spend cap, quiet hours; a proposal that would exceed any of them is rejected with a reason in the event bus |
| Permission drift: standing rules granted months ago still fire | Rules carry `expires_at` (3.1 §7.5); the kernel's maintenance process lists rules older than the partition's review period and asks for renewal; stale digests already stop a rule |
| Replay: a webhook, queue message or wake delivered twice runs twice | Idempotency keys on every wake (`hook:{name}:{id}` already in the ingress; `<source>:<key>` for the rest); the action ledger prevents a second execution of a completed tool call |
| Sleep/wake confusion: a run resumes under permissions that changed while it slept | Mode and rules are read at evaluation time, never cached on the run (3.1 I4); a mode loosening cancels held calls (3.1 I14) |
| The daemon as a new attack surface (OpenClaw's default-configuration findings) | Local socket trusts the OS user only; HTTP is loopback unless a token or SSO is configured; no plaintext credentials in manifests (`secret://` references through the broker); every control action is audited |
| A driver changes its tools after approval | Declaration digests pinned at mount; `NeedsReapproval` on change; standing rules stop matching |
| Cross-partition leakage | Policy class per partition; memory project scoping; `CoordinatorWithApproval` crossings audited |
| Freeze that does not reach a replica | `GovernanceControl::mirrored` with `max_staleness` and `fail_open: false` by default: a replica that cannot read the mirror treats itself as frozen |
| The ledger becomes surveillance | Content-free by construction: kinds, effects, durations and subject hashes only; never leaves the host without an export the user performs |

### 9.2 Invariants

**Process and wake.** K-I1 Every process is a durable run; no kernel code executes a tool outside a run. K-I2 A `Waiting` process holds no lease and causes no model call. K-I3 Every wake produces at most one run per idempotency key. K-I4 A resumed process never re-executes a completed non-idempotent call (3.1 I37). K-I5 `kill` is cooperative at the next checkpoint and never leaves a lease held; a hard kill is recovered by lease expiry.

**Governance.** K-I6 Every process runs under the harness `configure_run` layer; the kernel's own policy composes with it through `StrictestPolicy` and never loosens it. K-I7 Proactive and maintenance processes run with the partition's mode at most Ask unless the partition is explicitly Auto, and never above the proposing run's mode. K-I8 A freeze observed by any replica stops every process on that replica within one second and no process starts until unfreeze. K-I9 `PublishPublic` and `Financial` effect classes are never auto-allowed on any wake path.

**Proactivity.** K-I10 A proposal cannot run before it is `Approved` or admitted by an Auto partition's policy. K-I11 No proactive process starts inside quiet hours or beyond the daily budget. K-I12 The consolidation tick writes only `assumed` memory and `Proposed` agenda items.

**Drivers and partitions.** K-I13 A driver runs only with a manifest the user approved, under the recorded sandbox policy, with secrets resolved through the broker. K-I14 A tool whose declaration digest differs from the pinned one is `stale`: its standing rules do not match and it is `RequireApproval` until re-approved. K-I15 No agent reaches a partition's memory, drivers or sessions other than its own, except the coordinator under non-waivable approval.

**Control plane and surfaces.** K-I16 Every control action carries an authenticated subject and is audited. K-I17 Surfaces hold no state: disconnecting a surface changes nothing in the process table. K-I18 On a non-loopback bind, the control plane refuses to start without a bearer token or SSO extractor.

**Observation.** K-I19 The ledger contains no payloads, prompts, file contents or secrets. K-I20 An observation is emitted only after the warm-up, meets the "meaningful" rule, respects the cooldown, and is phrased as a question.

### 9.3 Audit records

Written through the signed audit log (3.1 §9.3), kind `Other` with `kernel.*` actions, actor from the control-plane subject or the process's acting context:

| Event | Action | Fields |
|---|---|---|
| Kernel lifecycle | `kernel.started` / `.paused` / `.resumed` / `.drained` / `.stopped` | kernelId, home, replicas, reason |
| Process | `kernel.process.{spawned,woken,suspended,resumed,killed,requeued}` | pid, parent, partition, queue, kind, wake, reason, by |
| Wake | `kernel.wake` | source, key, matchedPid or newPid, deduplicated |
| Agenda | `kernel.agenda.{proposed,approved,rejected,scheduled,expired}` | item, partition, origin, by, budget |
| Driver | `kernel.driver.{mounted,unmounted,started,crashed,restarted,stale,reapproved}` | driverId, commandDigest, partitions, effectClasses, digests, attempt |
| Partition | `kernel.partition.{created,changed,crossing}` | partition, fields, from, to, by |
| Governance | `kernel.freeze` / `kernel.unfreeze` | reason, by, replicasAcked |
| Control plane | `kernel.control` | subject, transport, action, target |
| Observation | `kernel.observation` | id, dimensions, windows (no values that identify content) |

## 10. Changes outside the new crate

| # | Crate | Change | Why |
|---|---|---|---|
| A1 (required) | adk-agent | `EventSource` adapters are public and reusable: expose `CronTrigger`, `FileWatchTrigger`, `WebhookTrigger` and the watermark types from `adk_agent::ambient` behind the existing feature, and let `TriggerEvent` carry an idempotency `key` (additive field with a default) | Wake sources reuse them ([`event_source.rs:11-36`](../../adk-agent/src/ambient/event_source.rs)) |
| S1 (required) | adk-server | `DurableWorker`, `Scheduler`, `BackgroundState`, `WebhookIngress` and `spawn_approval_ticker` are constructible outside the HTTP server (they are `pub`; confirm no `pub(crate)` constructor blocks composition) and expose lifecycle hooks (`on_claim`, `on_park`, `on_finish`) the kernel turns into `KernelEvent`s | [§7.2](#72-supervisor), [§7.6](#76-event-bus) |
| S2 (required) | adk-server | Control-plane router added to `ServerBuilder` via `add_api_routes`; `/runs` owner checks (spec 3.1 §8.2) are a prerequisite | [§7.11](#711-control-plane-and-surfaces) |
| T1 (required) | adk-tool | `McpServerManager` emits lifecycle events (`Starting`, `Running`, `Crashed`, `Restarting`, `Exhausted`, `Stopped`) on a subscribable channel, and accepts a `CommandLauncher` (spec 3.1 D1) so stdio servers can run sandboxed | [§7.7](#77-drivers); 3.1 §9 "unsandboxed stdio servers" |
| W1 (required, Windows) | adk-devtools, adk-tool | Process-tree kill through Job Objects for the shell and for drivers (the unix path is a process group, [`bash.rs:99, 159`](../../adk-devtools/src/tools/bash.rs)) | K-I5, 3.1 I23 on Windows |
| W2 (required, Windows) | adk-kernel | Named-pipe transport for the control plane | [§7.11](#711-control-plane-and-surfaces) |
| W3 (required, Windows) | adk-kernel | Windows Service installation (`windows-service`) | [§7.12](#712-service-installation) |
| W4 (required, Windows) | adk-kernel, adk-harness | Owner-only ACLs on kernel and harness stores (the unix path uses `0600`) | K-I16 |
| W5 (3.2.x, Windows) | adk-sandbox | A restricted-token-plus-ACLs shell enforcer as the first Windows tier, following Codex's ladder; AppContainer stays out ([`windows.rs:77-95`](../../adk-sandbox/src/sandbox/windows.rs) is a stub) | 3.1 §7.9 degraded mode on Windows |
| M1 (3.2.x) | adk-managed | `KernelDef` extension of `ManagedAgentDef` | [§7.13](#713-managed-form) |

No change to `DurableRunStore`, `RunRecord` or `NewRun`: the process side table avoids a schema change across six backends.

## 11. Testing and live verification

| Layer | Cases |
|---|---|
| Unit | Wake matching (every `Wake` variant, expiry actions); idempotency key derivation; admission (quota, budget, quiet hours) table; priority queue selection; preemption choice (youngest background); baseline statistics against fixtures (median/MAD, warm-up, cooldown, "meaningful"); manifest parsing and effect-class mapping; control-plane auth matrix |
| Property (`proptest`) | A wake never produces two runs for one key; admission never exceeds quota or budget under random event sequences; a `Waiting` process never has a lease; ledger rows never contain a payload string from the inputs; freeze is monotone (no process state goes to `Running` while frozen) |
| Conformance | `process_store_conformance_tests!`, `agenda_store_conformance_tests!`, `driver_store_conformance_tests!` over in-memory, file, SQLite |
| Integration (scripted model) | Every K scenario on `ScriptedLlm` with a real `DurableWorker`, `Scheduler`, `FileWatchTrigger`, `WebhookIngress`, `McpServerManager` and SQLite stores; chaos: `kill -9` the kernel mid-run and restart; two kernel replicas over one SQLite/Postgres store for K9; clock control for quiet hours and baselines |
| Live | K1–K5 and K8 on `gpt-5.6-terra`, `claude-sonnet-4-6` and `gemini-3.5-flash-lite`; K6 with a real MCP server; K9 on two replicas; latency assertions (wake under 2 s, scheduled within a tick); a 24-hour soak on this machine with the model-call counter asserted zero while asleep |
| Cross-platform | The scripted scenarios run on macOS and Linux CI; Windows runs K1–K5 with the shell in degraded mode until W5; service installation tested on each platform in a VM |

`examples/harness_kernel` holds the scenarios with a main-vs-branch table; `examples/harness_openai` and `examples/harness_anthropic` (from the 3.1 work) gain a `--kernel` mode that runs K2, K4 and K5 end to end on each provider.

## 12. Phased delivery

| Phase | Release | Contents | Size (engineer-weeks) | Demonstrates |
|---|---|---|---|---|
| 0 Prerequisites | 3.1.x | A1, S1, S2, T1; owner checks on `/runs`; 3.1 D1 (`CommandLauncher`) for sandboxed drivers | 2–3 | wake sources and supervision composable outside the server |
| 1 Kernel core | 3.2.0 | `Kernel` supervisor; process model and stores; wake sources (cron, file, webhook, approval, run-finished); event bus with SSE sink; control plane over Unix socket and loopback HTTP; `adk-rust up`, `attach`, `ps`, `kill`, `suspend`, `resume`, `events` | 6–7 | K1 (without service install), K2, K3, K5, K9 |
| 2 Proactivity and attention | 3.2.0 | Agenda, `propose_followup`, `ProactivityPolicy`, consolidation tick; priority queues, preemption, quotas, quiet hours, cost-aware admission; `agenda`, `nice`, `wake` commands | 3–4 | K4, K8 |
| 3 Drivers and partitions | 3.2.x | Manifests, mount approval, digest pinning, `NeedsReapproval`; partitions with memory scoping and crossings; `drivers`, `partitions` commands | 3–4 | K6, K7 |
| 4 `/proc`, service, Windows | 3.2.x | Ledger, baselines, observations; launchd/systemd/Windows service; W1–W4 | 4–5 | K1 (reboot), K10 |
| 5 Managed form and channels | 3.2.x / 3.3 | `KernelDef`; channel wake sources once channels land; W5 | 3–4 | tenants; message-driven wakes |

Total about 21–27 engineer-weeks; estimates for one engineer.

## 13. Open questions

1. Should the consolidation tick be on by default in Ask partitions (it only proposes), or opt-in?
2. Priority classes as queues means three workers per kernel; is a single worker with ordered claiming (a `DurableRunStore::claim` priority argument, a core change across six backends) worth it for 3.3?
3. Partition crossing by the coordinator: approval per request, or a standing rule with an expiry?
4. Should `/proc` baselines live in the kernel or in `adk-eval` as an analyzer over the ledger?
5. Does Agentrix OS migrate onto the kernel in 3.2.x, or wait for channels in 3.3?

## 14. Appendix: grounding against current APIs (`origin/main` @ `a24ebd8d`)

| Area | Items |
|---|---|
| Durable runs | `DurableRunStore { enqueue, claim, heartbeat, set_checkpoint, wait, wake, finish, request_cancel, get, list, prune }` [`run_store.rs:620`](../../adk-core/src/run_store.rs); `RunState` 88; `RunCheckpointRef::AwaitingApproval` 137-148; `NewRun { queue, app_name, user_id, session_id, target, input, idempotency_key, concurrency_key, on_conflict, not_before, max_attempts, principal }` 189-220; `RunRecord` 360-428 |
| Worker | `DurableWorkerConfig { worker_id, queue, lease_ttl, poll_interval, max_concurrent_runs, run_timeout, retain_finished, approval_timeout, allowed_principals }` [`worker.rs:122-145`](../../adk-server/src/background/worker.rs); `with_queue` 172; `run_agent` 789-979; `Attempt::Parked` 277 |
| Scheduler | `Schedule`, `MissedFirePolicy`, `ScheduleStore` [`schedule.rs:97, 132, 450`](../../adk-core/src/schedule.rs); `Scheduler::new`/`tick` [`scheduler.rs:99, 152`](../../adk-server/src/background/scheduler.rs); `ConcurrencyPolicy`, `CronState` [`cron.rs:49, 173-194`](../../adk-server/src/background/cron.rs); `BackgroundState::{new, with_run_store, with_executor, with_agent}` [`background/mod.rs:529-558`](../../adk-server/src/background/mod.rs); `WorkflowExecutor` 269 |
| Ambient triggers | `AmbientAgent::{new, with_max_concurrent_triggers, take_output, with_trigger_handler, start, stop, pause, resume, status}` [`ambient/agent.rs:79-332`](../../adk-agent/src/ambient/agent.rs); `EventSource`, `TriggerEvent` [`event_source.rs:11-36`](../../adk-agent/src/ambient/event_source.rs); `CronTrigger` [`cron_trigger.rs:24, 73-127`](../../adk-agent/src/ambient/cron_trigger.rs); `FileWatchTrigger` [`file_watch_trigger.rs:22-36`](../../adk-agent/src/ambient/file_watch_trigger.rs); `WebhookTrigger` [`webhook_trigger.rs:143`](../../adk-agent/src/ambient/webhook_trigger.rs); `TriggerSessionPolicy { PerTrigger, Shared }` and the runner bridge [`runner_bridge.rs:21-146`](../../adk-agent/src/ambient/runner_bridge.rs); watermarks [`watermark.rs:62`](../../adk-agent/src/ambient/watermark.rs) |
| Ingress and queues | `WebhookIngress::{new, with_hook, with_governance, with_retry_after, with_body_limit}`, `WebhookHook`, `HookAccepted`, `webhook_ingress_router` [`ingress.rs:72-554`](../../adk-server/src/webhooks/ingress.rs); `QueueSource { name, pull, ack, nack }` and the in-memory source and worker [`queue_source.rs:112-427`](../../adk-core/src/queue_source.rs) |
| Governance | `GovernanceControl::{mirrored, freeze, unfreeze, is_frozen}`, `GovernanceMirrorConfig`, `GovernanceWatch` [`governance.rs:111-477`](../../adk-core/src/governance.rs); `authorize_tool_call` 1527 |
| Drivers | `McpServerManager::{new, start_server, stop_server, restart_server, start_monitoring, stop_monitoring, add_server, start_all, shutdown}` [`manager.rs:61-1224`](../../adk-tool/src/mcp/manager/manager.rs); `McpServerConfig`, `McpLifecycleMode`, `RestartPolicy` [`config.rs:14-143`](../../adk-tool/src/mcp/manager/config.rs) |
| Identity, budgets, spend | `AgentPrincipal { id, tenant, class, .. }` [`principal.rs:245`](../../adk-core/src/principal.rs); `PolicyRule::for_class` [`policy.rs:547`](../../adk-core/src/policy.rs); `RunBudget` [`budget.rs:105`](../../adk-core/src/budget.rs); `SpendLedger`, `SpendKey` [`spend.rs:105, 376`](../../adk-core/src/spend.rs); project-scoped memory [`adk-memory/src/service.rs:51`](../../adk-memory/src/service.rs) |
| Managed | `ManagedAgentDef` [`agent_def.rs:36-61`](../../adk-managed/src/types/agent_def.rs); `ManagedAgentRuntime` [`runtime.rs:194`](../../adk-managed/src/runtime.rs); `resume_parked` [`session_loop.rs:613, 661`](../../adk-managed/src/session_loop.rs) |
| Platform | `notify` 8 [`adk-agent/Cargo.toml:41`](../../adk-agent/Cargo.toml); keyring backends per platform [`adk-cli/Cargo.toml:56-62`](../../adk-cli/Cargo.toml); unix-only process group and file code [`bash.rs:99, 159`](../../adk-devtools/src/tools/bash.rs), [`workspace.rs:281-407`](../../adk-devtools/src/workspace.rs); Windows enforcer stub [`windows.rs:77-95`](../../adk-sandbox/src/sandbox/windows.rs) |
| Spec 3.1 | `HarnessAgent::configure_run`, `HarnessMode`, `ModeStore`, `StandingRuleStore`, `Harness::goal_target`, `ToolKind`, the audit record set ([`spec-3.1-harness-agent.md`](spec-3.1-harness-agent.md) §7.3–§7.5, §7.11, §9) |

**Net new code:** the `adk-kernel` crate (supervisor, process table and stores, wake sources, agenda and proactivity, event bus, drivers, partitions, scheduling, `/proc`, control plane, service installation), the CLI commands, and the additive changes in [§10](#10-changes-outside-the-new-crate). Everything else is composition of APIs that exist at `a24ebd8d`.

## References

- Codex persistent mode and proactivity: [Techstrong report](https://techstrong.ai/agentic-ai/openai-testing-persistent-mode-to-give-codex-always-on-autonomous-coding-capabilities-report/), [Codex Cloud on web and phone](https://pasqualepillitteri.it/en/news/19386/codex-cloud-web-mobile-devday-2026)
- Persistent agents as a 2026 direction: [Business Standard](https://www.business-standard.com/technology/artificial-intelligence/rise-of-persistent-ai-agents-openai-meta-google-126100200565_1.html), [ByteByteGo](https://blog.bytebytego.com/p/whats-next-in-ai-five-trends-to-watch)
- OpenClaw gateway architecture and security: [2026 guide](https://vallettasoftware.com/blog/post/openclaw-2026-guide), [architecture](https://ramnode.com/guides/series/openclaw/introduction-architecture), [deep dive](https://medium.com/@dingzhanjun/deep-dive-into-openclaw-architecture-code-ecosystem-e6180f34bd07)
- Cloudflare Computer: [changelog, 2026-08-03](https://developers.cloudflare.com/changelog/post/2026-08-03-cloudflare-computer/), [InfoQ](https://infoq.com/news/2026/08/cloudflare-computer-agents)
- Agent kernels in research: [AIOS](https://arxiv.org/abs/2403.16971), [AgentOS](https://arxiv.org/pdf/2603.08938)
- Sleep-time agents: [Letta blog](https://letta.com/blog/sleep-time-compute), [Letta docs](https://docs.letta.com/guides/agents/architectures/sleeptime)
- Always-on risk: [OWASP Top 10 for Agentic Applications summaries](https://securew2.com/blog/owasp-top-10-agentic-ai), [Group-IB](https://www.group-ib.com/resources/knowledge-hub/agentic-ai-security-risks/)
- Agentrix OS: [repository](https://github.com/zavora-ai/agentrix-os), `docs/PERSONAL_AI_OS.md` §1.3, §2.5, §6.1, §7, §8, §10
