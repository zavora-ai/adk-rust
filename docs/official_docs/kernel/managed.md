# The managed form

The [agent kernel](index.md) hosts a user's or a team's agents on their own infrastructure;
the [managed runtime](../managed-agents/runtime.md) hosts tenants. The managed form joins the
two (spec 3.2 §7.13, §10 M1): a hosted definition carries a `KernelDef` — wake sets, a
proactivity policy, driver manifests and partitions — and the runtime runs one kernel per
tenant over that tenant's `ManagedStores`. A wake starts a governed run of the tenant's agent,
a held call parks on the tenant's run store, and a decision through the tenant's approval
service wakes it.

`adk-managed` is Experimental, so the managed form is too.

```toml
[dependencies]
adk-kernel = { version = "3.0.0", features = ["managed"] }
adk-harness = { version = "3.0.0", features = ["managed", "kernel"] }
adk-managed = { version = "3.0.0", features = ["anthropic"] }
adk-core = "3.0.0"
adk-session = "3.0.0"
serde_json = "1"
tokio = { version = "1", features = ["full"] }
```

## A definition with a kernel block

The block travels as JSON under the `adk.kernel` metadata key of a `ManagedAgentDef`, beside
the harness's `adk.harness`, so every store that keeps a definition — the managed state
store's session records included — keeps it unchanged. It is written in the kernel's own
camelCase wire shape: its wakes are `Wake`s, its drivers `DriverManifest`s and its policy a
`ProactivityPolicy`.

```json
{
  "wakeSets": [
    {
      "name": "orders",
      "anyOf": [{ "webhook": { "hook": "orders" } }, { "cron": { "expr": "0 0 9 * * *", "missed": "skip" } }],
      "input": "Triage the new orders into orders.md.",
      "overlap": "skip"
    }
  ],
  "proactivity": {
    "maxProposedOpen": 5,
    "quietHours": { "start": "22:00", "end": "07:00" },
    "approval": "always_ask"
  },
  "drivers": [
    {
      "id": "email",
      "server": { "command": "mcp-email", "env": { "EMAIL_TOKEN": "secret://email/token" } },
      "partitions": ["default"],
      "sandbox": "os",
      "tools": { "list_inbox": "read", "send_draft": "send_external" }
    }
  ]
}
```

| Field | In the tenant's kernel |
|---|---|
| `wakeSets` | Standing triggers named `<definition>/<set>`: each wake starts a process of the definition's agent with `input`, on `queue` (default `background`) for `user` (default `kernel`) as a `Service` subject, keyed by the wake's own key so a repeated delivery starts nothing |
| `wakeSets[].overlap` | `skip` (default): a firing while the set's previous process is unfinished starts nothing; `queue`: it queues behind it |
| `wakeSets[].anyOf` | `cron`, `webhook`, `queue` and `signal` wakes ([which wakes a wake set takes](#which-wakes-a-wake-set-takes)) |
| `proactivity` | The tenant kernel's `ProactivityPolicy`: open proposals, the daily proactive budget, quiet hours and how proposals are approved. One kernel has one policy, so a second definition may repeat it or leave it out |
| `proactivity.consolidation` | The only switch for the consolidation tick: it is off in the managed form unless a definition sets it (§13 Q1), and runs where the operator registered a consolidator |
| `drivers` | `DriverManifest`s, validated and recorded; no driver is mounted without a person's approval |
| `partitions`, `coordinator` | One agent per partition — one harness per partition with `HarnessResolver` as the tenant's `partition_resolver` — as `<definition>@<partition>`, each partition in the tenant's partition table with the tenant as its tenant; the coordinator's agent serves the definition's own target. Without a partition resolver the partitions are recorded and the agent runs in the default partition ([partitions](partitions.md#partitions-in-the-managed-form)) |

`KernelDef::from_agent_def` reads and validates a block; `KernelDef::apply_to` writes one into
a definition, keeping its other metadata keys.

## Hosting tenants

```rust
use std::sync::Arc;

use adk_core::{ApprovalPolicy, ApproverSelector, InMemoryApprovalStore, InMemoryRunStore, StoreApprovalService};
use adk_harness::HarnessMode;
use adk_harness::packs::managed::{HarnessDef, HarnessResolver, ShellPolicy};
use adk_kernel::Wake;
use adk_kernel::managed::{KernelDef, ManagedKernels, TenantKernel, WakeSetDef};
use adk_managed::resolver::DefaultModelResolver;
use adk_managed::types::{ManagedAgentDef, ModelRef};
use adk_managed::{InMemoryManagedStateStore, ManagedAgentRuntime, ManagedStores};
use adk_session::InMemorySessionService;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The tenant's own stores, its approval service routing held calls to the operator
    // group and waking the parked run on a decision.
    let runs = Arc::new(InMemoryRunStore::new());
    let decisions = Arc::new(InMemoryApprovalStore::new());
    let approvals = Arc::new(
        StoreApprovalService::new(decisions.clone())
            .with_run_store(runs.clone())
            .with_router(ApprovalPolicy::new([ApproverSelector::group("operator")])),
    );
    let stores = ManagedStores::new(Arc::new(InMemoryManagedStateStore::new()), runs, decisions)
        .with_approval_service(approvals);

    let tenant = TenantKernel::builder("tenant-a", stores, Arc::new(InMemorySessionService::new()))
        .resolver(Arc::new(HarnessResolver::new("/srv/workspaces/tenant-a")))
        .build()?;
    let runtime = tenant.runtime(Arc::new(DefaultModelResolver::new()));

    let harness: ManagedAgentDef = HarnessDef::new("triage", ModelRef::Shorthand("claude-sonnet-5-5".into()))
        .with_mode(HarnessMode::Ask)
        .with_shell(ShellPolicy::none())
        .into();
    let def = KernelDef::new()
        .with_wake_set(WakeSetDef::new(
            "orders",
            [Wake::Webhook { hook: "orders".into() }],
            "Triage the new orders into orders.md.",
        ))
        .apply_to(harness);
    runtime.create(def).await?;

    let kernels = ManagedKernels::new();
    kernels.insert(tenant)?;
    kernels.start().await?;
    tokio::signal::ctrl_c().await?;
    kernels.stop().await?;
    Ok(())
}
```

`TenantKernel::resolver` is the `AgentResolver` the tenant's runtime is built with
(`DefaultManagedAgentRuntime::with_agent_resolver`); `TenantKernel::runtime` builds the
runtime with it, the tenant's stores and its session service. A definition is hosted when the
runtime resolves it — on `create`, and when a session is rebuilt from the state store — so every
runtime instance of the tenant needs a `TenantKernel` over the same stores.

| Kernel part | From the tenant |
|---|---|
| Run store | `ManagedStores::runs`: the kernel's queues (`interactive`, `background`, `maintenance`) beside the sessions' per-session queues |
| Approvals | `ManagedStores::approval_service`, whose run store wakes a parked process, and `ManagedStores::decisions` as each run's approval store |
| Targets | One per hosted definition, named like it: the agent the operator's resolver builds |
| Triggers | The definitions' wake sets; one cron source per expression |
| App name | The tenant: the `ManagedOwner::app_name` its sessions are created under |
| Kill switch | Its own `GovernanceControl`, unless the builder is given one |
| Agenda | The tenant's agenda store; `TenantKernel::agenda_handle` is bound to each kernel the tenant builds, and `HarnessAgentBuilder::agenda_handle` gives it to the tenant's harnesses for `propose_followup` |
| Signals | With the `charter` feature, the tenant's signal store (`TenantKernelBuilder::signal_store`); `TenantKernel::signal_sink` files signals ([signal wakes](#signal-wakes)) |
| Control plane | None of its own; mount `adk_kernel::control::router(kernel)` on the host's server |

A definition hosted after `TenantKernel::start`, or hosted again changed, restarts the tenant's
kernel over the same stores: its running processes park at their next model-call boundary and
the new kernel resumes them. The same definition hosted again changes nothing.

`ManagedKernels` holds a host's tenants, one kernel each, and refuses a tenant whose run store,
decision store, state store, approval service, process store, partition store, cron watermark
store, signal store, driver supervisor or driver store another tenant holds: a shared run store
would let one tenant's workers claim the other's runs. Distinct store values over one
database are not detected, so give each tenant its own database, schema or key namespace.

## Which wakes a wake set takes

A wake set is standing: every firing starts a process. A wake that only ever wakes one sleeping
process is refused, with a message naming what to use instead.

| Wake | In a wake set |
|---|---|
| `cron`, `webhook`, `queue` | Accepted. The host serves a webhook with `webhook_wake_hook` and a queue with `queue_wake_ingress` on the tenant's run store |
| `signal` | Accepted: `kind` is an identifier (letters, digits, `-`, `_`); `partition` is one the block declares, or unset for the wake set's own partition. The tenant needs a signal store |
| `fileChange` | Refused: it would watch the host's files |
| `at` | Refused: it matches no event, only a sleeping process's time. Recurring work is a `cron` wake; one-shot work is a schedule (`ScheduleSpec::At`) on the tenant's schedule store (`TenantKernelBuilder::schedule_store`) |
| `budgetReplenished` | Refused: admission already parks a budget-deferred process on it and wakes it when the budget returns, so as a standing wake it never fires |
| `approval`, `runFinished`, `manual` | Refused: each names one sleeping process |

## Signal wakes

A wake set hears a [signal](charter.md) kind in one partition (spec 3.5 §7.10, feature
`charter`). The host files signals through the tenant's sink:

| Part | What it does |
|---|---|
| `TenantKernelBuilder::signal_store` | The tenant's own `SignalStore`; `ManagedKernels` refuses one another tenant holds |
| `TenantKernel::signal_sink(partition)` | A `SignalSink` over the tenant's signal store, agenda store, run store, audit trail and clock, filing in `partition` — the default partition, or one in the tenant's partition table |
| Hosting | A definition with a signal wake on a tenant without a signal store is refused, naming the definition, the wake set and `TenantKernelBuilder::signal_store` |

Each signal filed or counted becomes a keyed `kernel.wake` run on the tenant's run store,
`signal:<partition>:<item>:<count>`, which the kernel routes to every wake set hearing that kind
in that partition:

- **Own partition only:** an unset `partition` is the wake set's own, and a partition the block
  declares is the definition's alone, so a definition never hears another definition's signals.
- **Unauthenticated:** a signal carries no principal, so its run is not authenticated and the
  harness's standing rules and Auto stay inert for it (spec 3.1 I6).
- **The definition's input:** the run starts with the wake set's `input`; the signal's typed
  fields stay on the back office's agenda item, never in the run's input (spec 3.5 A-I3).
- **Once per key:** the run store keeps one run per key, so a repeated delivery starts nothing
  (K-I3); a counted repeat of a signal is a new key and wakes the set again under its `overlap`.
- **Across restarts:** the sink enqueues on the tenant's run store, not on one kernel, so a sink
  taken before a definition is hosted — and the tenant's kernel restarts — wakes the new kernel.
  It publishes on no event bus, since each kernel the tenant builds has its own.

```rust
use std::sync::Arc;

use adk_kernel::Wake;
use adk_kernel::charter::InMemorySignalStore;
use adk_kernel::managed::{KernelDef, TenantKernel, WakeSetDef};
use adk_managed::types::{ManagedAgentDef, ModelRef};
use adk_managed::{AgentResolver, ManagedAgentRuntime, ManagedStores};
use adk_managed::resolver::DefaultModelResolver;
use adk_session::InMemorySessionService;
use awp_types::Signal;

async fn writer(harnesses: Arc<dyn AgentResolver>, signal: Signal) -> Result<(), Box<dyn std::error::Error>> {
    let tenant = TenantKernel::builder("tenant-a", ManagedStores::in_memory(), Arc::new(InMemorySessionService::new()))
        .resolver(harnesses)
        .signal_store(Arc::new(InMemorySignalStore::new()))
        .build()?;
    let runtime = tenant.runtime(Arc::new(DefaultModelResolver::new()));
    let gaps = WakeSetDef::new(
        "gaps",
        [Wake::Signal { kind: "content_gap".into(), partition: None }],
        "Write the page the visitors asked for.",
    );
    let def = KernelDef::new()
        .with_wake_set(gaps)
        .apply_to(ManagedAgentDef::new("writer", ModelRef::Shorthand("gemini-3.7-flash".into())));
    runtime.create(def).await?;
    tenant.start().await?;
    // A content gap the public partition saw starts one run of `writer`.
    tenant.signal_sink("default").await?.emit(signal).await?;
    Ok(())
}
```

> **Note:** `SignalStore`, like the other charter stores, has in-memory, SQLite, PostgreSQL and
> Redis backends only. A tenant whose other stores are on MongoDB, Firestore or Neo4j keeps its
> signals on one of those four.

## A definition's drivers

A definition's partitions mount its drivers on the tenant's own `DriverSupervisor`
(`TenantKernelBuilder::drivers`), only once a person approves each one (spec 3.2 §7.7, §7.8,
K-I13):

| Step | What happens |
|---|---|
| 1. Hosting | Each driver a declared partition mounts — the partition's `drivers` names it and the manifest's `partitions` lists the partition — is proposed in the tenant's approval inbox as a request with `tool_name` `kernel:driver.mount` (`DRIVER_MOUNT_TOOL_NAME`). The request shows what the person approves: the command line, the partitions, the effect classes, the sandbox and the `secret://` references |
| 2. A person decides | An approver in the `operator` group, or the policy `TenantKernelBuilder::driver_approvers` sets, approves or rejects the request in the tenant's inbox |
| 3. The mount | Every scheduler tick while the tenant's kernel runs, `TenantKernel::apply_driver_decisions` mounts each approved driver as the person who approved it (`kernel.driver.mounted`); an approval by anyone but a person mounts nothing |
| 4. Pinning | The first listing of the driver's tools pins each tool's declaration digest; a later change makes the tool stale until `DriverSupervisor::reapprove` (K-I14) |
| 5. Serving | Each partition's harness mounts the drivers its `drivers` names (`HarnessResolver` calls `partition_drivers`); the coordinator reaches the definition's other partitions' drivers only as [crossings](partitions.md#crossing-into-another-partitions-drivers) |

A changed manifest — another command, partitions, class, sandbox or secret — is proposed again
under a new request; the approved one keeps serving until the new one is approved. A driver no
declared partition mounts is recorded and not proposed: drivers reach a definition's agents
through its partitions, so a definition without partitions mounts none.

Each tenant's drivers are invisible to every other tenant: a tenant's supervisor and driver store
are its own (`ManagedKernels` refuses a tenant whose supervisor or driver store another tenant
holds), its approvals are decided in its own inbox, and its drivers run as the tenant's app under
`<kernel home>/tenants/<tenant>-<digest>` unless the supervisor's builder set a home.

```rust
use std::sync::Arc;

use adk_core::{ApprovalPolicy, ApproverSelector};
use adk_harness::packs::managed::HarnessResolver;
use adk_kernel::drivers::DriverSupervisor;
use adk_kernel::managed::TenantKernel;
use adk_kernel::stores::InMemoryDriverStore;
use adk_managed::ManagedStores;
use adk_session::InMemorySessionService;

fn tenant(stores: ManagedStores) -> adk_kernel::Result<TenantKernel> {
    let harnesses = Arc::new(HarnessResolver::new("./workspaces/tenant-a"));
    TenantKernel::builder("tenant-a", stores, Arc::new(InMemorySessionService::new()))
        .resolver(harnesses.clone())
        .partition_resolver(harnesses)
        // The tenant's own supervisor over the tenant's own driver store.
        .drivers(DriverSupervisor::builder(Arc::new(InMemoryDriverStore::new())).build())
        .driver_approvers(ApprovalPolicy::new([ApproverSelector::group("tenant-a-admins")]))
        .build()
}
```

## Cron wakes missed while a tenant was down

Each cron expression of a tenant's wake sets has one cron source, which keeps its cursor in the
tenant's own `CronWatermarkStore` (`TenantKernelBuilder::watermark_store`; in memory by default,
which keeps nothing across a restart). When a replica of the tenant starts after an outage, the
ticks no replica watched fire once each, as the wake set's `missed` policy picks them (spec 3.2
§7.4):

| `missed` | Missed ticks that fire |
|---|---|
| `skip` | The latest, only when it is no more than a minute late |
| `fireOnce` | The latest, however late |
| `catchUp(n)` | The latest `n`, oldest first |

The cursor is written before each tick goes out, so a restart replays nothing a second time, and
each tick keeps its key (`cron:<source>:<time>`), so replicas that start together fire each tick
once (K-I3). A catch-up's processes follow the wake set's `overlap`: they queue behind each other
(`queue`), or the later ones start nothing while one is unfinished (`skip`), so a long outage is
never a burst. A source with no cursor yet starts at its first watch and replays nothing from
before it existed. When a definition hosted again leaves no wake set on an expression, the tenant
deletes that expression's cursor: the time it is idle is not missed, so a wake set that uses it
later starts at its first watch. The tenant kernel's sleepers keep their cron cursors in the same
store ([backends](backends.md#cron-sleepers)). The store runs on in-memory, SQLite, PostgreSQL and Redis, and on
MongoDB, Firestore and Neo4j ([backends](backends.md));
`ManagedKernels` refuses a tenant whose watermark store another tenant holds.

## What a tenant's definition cannot do

A definition comes from a tenant, so its kernel block cannot widen what the operator allows.

| Limit | Refused, naming the key |
|---|---|
| The agent | A definition with a kernel block runs only an agent the operator's resolver builds, such as a harness definition under `HarnessResolver`; a plain definition stays in the session loop |
| Drivers | `sandbox: "unsandboxed"`; pinned `declarationDigests`; a credential-named variable with a plain value instead of a `secret://` reference; a partition the block does not declare |
| Wake sets | The `interactive` queue a person's turn holds; a `fileChange` wake on the host's files; approval, run-finished, time, budget and manual wakes, which start no standing work ([which wakes a wake set takes](#which-wakes-a-wake-set-takes)); an unparsable cron expression; a signal kind that is not an identifier, or a signal partition the block does not declare |
| Partitions | A `tenant` key: the tenant is the hosting kernel's; an id declared twice; an unknown key; a `coordinator` the block does not declare; a partition another hosted definition of the tenant declares |
| Consolidation | No field lets an agenda item of origin `Consolidation` start without a person's approval, in any mode (K-I23) |

## The managed approval path

| Step | What happens |
|---|---|
| 1. A wake | A webhook delivery (served with `webhook_wake_hook` on the tenant's run store), a queue message, a cron tick or a signal filed through `TenantKernel::signal_sink` becomes a keyed `kernel.wake` run that the maintenance worker routes to the wake set's trigger |
| 2. A held call | The process runs under the harness's mode; a call that needs approval is submitted to the tenant's approval service with the run's id, and the run parks `Waiting` on the tenant's run store |
| 3. A decision | An approver decides the request in the tenant's inbox; the service's run store wakes the run |
| 4. The call runs once | The kernel's worker resumes the process, the call runs under the action ledger, and a restart runs nothing again (K-I4) |

## Voice decisions on parked processes

The [voice surface](../harness/voice.md) answers a parked process through the same approval
service (spec 3.2 §7.11). `VoiceSurface::with_parked_processes` gives it the kernel's process
table and the service; `parked_processes` lists the processes waiting on a request the voice
user may decide; `offer_parked` puts one up for the user's next plain yes or no, which is
decided through the service and wakes the run. The session's own held calls are answered first.
A process a freeze parked is refused with `harness.voice.frozen` and a `harness.voice.refused`
record: only `unfreeze` wakes it (K-I21).

## Testing

| Where | Proves |
|---|---|
| `adk-kernel/tests/managed_def_fixtures.rs`, `tests/fixtures/managed/` | Twenty golden fixtures: each block's canonical form with its defaults explicit, the consolidation switch, the triggers a tenant kernel registers, and every refusal's message, the signal wake and the refused time and budget wakes among them |
| `adk-kernel/tests/managed_signals.rs` (features `managed`, `charter`) | A filed signal starts the definition unauthenticated with its own input, keyed `signal:<partition>:<item>:<count>`, and a repeated key starts nothing; a definition hears only its own partition's signals; a sink taken before the kernel restarts wakes the new kernel; a signal wake on a tenant without a signal store is refused; tenants never share a signal store |
| `adk-kernel/tests/managed_tenants.rs` | A definition with its block round-trips through the managed state store unchanged and is hosted again on a rebuilt session; a cron wake for tenant A runs on A's stores only and nothing reaches tenant B; a held call runs once under the action ledger after the tenant's approval; a definition hosted after start restarts the kernel; one policy per tenant, the tick off unless a definition turns it on; a consolidation item waits for a person in Auto; no tenant kernel opens a control plane |
| `adk-kernel/tests/managed_tenant_property_tests.rs` | Over random deliveries, approvals and approvals attempted through another tenant's service, across two or three tenants sharing a session service: no wake, process, approval or session crosses tenants (`proptest`, 100 cases) |
| `adk-harness/tests/managed_kernel_tests.rs` | A harness definition on a tenant kernel: the held write runs once after the tenant's approval, and the harness's follow-up lands on the tenant's agenda |
| `adk-harness/tests/voice_kernel_tests.rs` | A scripted spoken yes wakes a parked process; a freeze-parked process is refused and wakes only on unfreeze |
| `adk-kernel/tests/managed_drivers.rs` | A definition's driver is proposed in each tenant's own inbox, mounted only when a person approves, pinned at its first listing, run under the tenant's own home; another tenant's approval mounts nothing; a changed manifest is asked again; an approval by a service mounts nothing; tenants never share a supervisor or a driver store |
| `adk-harness/tests/managed_partition_drivers.rs` | With real harnesses: tenant A's approved driver serves A's work partition, the coordinator reaches it only as a crossing its process parks on until approved, and tenant B, hosting the same definition, is offered nothing |
| `adk-kernel/tests/managed_cron_watermarks.rs`, `cron_watermark_store_sqlite.rs` | Missed ticks fire once each as `skip`, `fireOnce` and `catchUp(2)` pick them; a restart replays nothing; two replicas starting together fire each tick once; each tenant keeps its own cursors; an expression hosted again after it was idle replays nothing from the idle time; the store's conformance suite in memory and on SQLite (PostgreSQL and Redis in CI's `postgres` job) |
| `examples/harness_kernel` `m1_one_kernel_per_tenant`, `m2_a_tenants_drivers`, `m3_missed_ticks_replay_once`, `v1_voice_answers_a_parked_process`, `live_managed_tenant_approval`, `live_managed_tenant_drivers`, `live_tenant_missed_ticks`, `live_voice_parked_process` | The managed form with two tenants scripted and one tenant live on `claude-sonnet-5-5` and `gemini-3.5-flash-lite`; a parked process decided by voice, scripted and live on Gemini Live |

## Related

- [The agent kernel](index.md)
- [Drivers](drivers.md)
- [Serving the harness on the managed runtime](../harness/serving.md#on-the-managed-runtime)
- [Harness voice](../harness/voice.md)
- Spec: `docs/design/spec-3.2-agent-kernel.md` §7.11, §7.13, §10 M1, §13 Q1
