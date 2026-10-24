# Partitions

A partition keeps one area of a user's life or one team's work apart from the others: work and
home, or two teams on one host (spec 3.2 §7.8). Each partition is a policy class, a memory scope
and a set of drivers, run by a harness of its own. No agent reaches another partition's memory,
drivers or sessions, except the coordinator, and only under an approval no standing rule can give
(K-I15).

| Scenario (K7) | Result |
|---|---|
| A work agent asks for a home calendar entry | Denied at the policy; nobody is asked; the attempt is audited as `kernel.partition.crossing` |
| The coordinator asks for the same entry | Held for a person, who sees the crossing named and three answers; the decision is audited |
| The coordinator asks again in the same run | Runs without asking, when the first was an approved read |
| The coordinator writes into the other partition | Asked again, every time |
| The next run | Asks again |

## The partition record

| Field | Default | Meaning |
|---|---|---|
| `id` | — | `work`, `home`, a team |
| `class` | — | `AgentPrincipal.class` of every agent in the partition, so `PolicyRule::for_class` rules apply to it |
| `tenant` | none | `AgentPrincipal.tenant` on multi-tenant hosts |
| `defaultMode` | `ask` | The mode of the partition's harness: `read_only`, `plan`, `ask`, `auto` |
| `memoryScope` | the id | The project of the partition's long-term facts and project-scoped memory |
| `drivers` | none | The ids of the drivers mounted for the partition |
| `quotas` | none | `maxRunning`, `maxQueued`, `dailySpendCap` (micro-USD) for the partition's processes; each applies beside the kernel's quota, the lower of the two |
| `quietHours` | none | A window in which no proactive or maintenance process of the partition starts, held beside the kernel's windows |
| `crossPartition` | `deny` | `deny`: no crossing into or out of the partition; `coordinator_with_approval`: the coordinator may cross, each crossing under approval |
| `blockedCrossings` | none | Partitions this partition's coordinator may no longer cross into ("deny and block") |

A crossing needs both partitions to take part: a partition set to `deny` is isolated both ways,
even from the coordinator. Agentrix's Work and Home worlds are two partitions with
`coordinator_with_approval`.

## One harness per partition

`HarnessAgentBuilder::partition` builds the harness of one partition. Give every partition's
harness the same `Partitions` table and the same fact store; `partition_coordinator` marks the one
that may cross.

```toml
[dependencies]
adk-rust = { version = "3.0.0", features = ["harness-kernel"] }
adk-devtools = "3.0.0"
```

```rust
use std::sync::Arc;

use adk_rust::harness::{Harness, HarnessAgent, InMemoryFactStore, LongTermMemoryConfig};
use adk_rust::kernel::{CrossPartition, Partition, PartitionMode, Partitions};
use adk_rust::session::{InMemorySessionService, SessionService};
use adk_rust::{Llm, audit_log::AuditActor};

async fn partitions(model: Arc<dyn Llm>) -> adk_rust::Result<(Harness, Harness)> {
    let table = Partitions::in_memory();
    let by = AuditActor::default();
    table
        .put(
            Partition::new("home", "home-agents")
                .with_cross_partition(CrossPartition::CoordinatorWithApproval),
            by.clone(),
        )
        .await?;
    table
        .put(
            Partition::new("work", "work-agents")
                .with_mode(PartitionMode::Auto)
                .with_driver("calendar")
                .with_cross_partition(CrossPartition::CoordinatorWithApproval),
            by,
        )
        .await?;
    let facts = Arc::new(InMemoryFactStore::new());
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());

    let build = |id: &str, coordinator: bool| {
        let table = table.clone();
        let facts = facts.clone();
        let model = model.clone();
        let sessions = sessions.clone();
        let id = id.to_string();
        async move {
            let partition = table.get(&id.as_str().into()).await?.expect("the partition exists");
            let mut builder = HarnessAgent::builder("harness")
                .model(model)
                .workspace(adk_devtools::Workspace::new(format!("./{id}")))
                .long_term_memory(LongTermMemoryConfig::default())
                .fact_store(facts)
                .partition(partition, table);
            if coordinator {
                builder = builder.partition_coordinator();
            }
            builder.build()?.into_harness("adk-rust", sessions)
        }
    };
    let home = build("home", true).await?;
    let work = build("work", false).await?;
    Ok((home, work))
}
```

What the builder sets for the partition:

| Part | What the partition's harness does |
|---|---|
| Mode | The partition's `defaultMode` is the harness mode; call `partition` after `mode`, which it replaces |
| Identity | Every agent of the harness's runner, team members included, acts as `urn:adk:agent:{app}/{agent}` with the partition's class and tenant and the attribute `adk.partition`, through the runner's identity source (`Partition::identity_source`) |
| Long-term facts | `FactScope.project` is the partition's `memoryScope` for every kind, preferences included. Recall, `memory_search` and the person's fact listing read that scope only, never a user-wide fact, and a fact of another partition cannot be superseded by id |
| Sessions | The harness marks each session it runs as its partition's (`harness:partition`, `PARTITION_STATE_KEY`) and refuses, with `harness.partition_session`, a session another partition's harness ran |
| Consolidation | The partition's consolidation tick works on the partition's facts only, never a user-wide fact |
| Follow-ups | `propose_followup` puts the item on the partition's agenda; approved, it runs as a process of the partition on its harness |
| Working notes | A memory root of the partition's own: `<base>/partitions/<memoryScope>/…`, and `<workspace>/.adk/memory/partitions/<memoryScope>/…` for the project layout |
| Project-scoped memory | `Partition::scope_memory` confines an `adk_core::Memory` to the partition's project; a search or a write naming another project is refused, and a memory without project scoping is refused outright |
| Drivers | `partition_drivers(&supervisor)`: the kernel's [drivers](drivers.md) mounted for the partition — `kernel_drivers` with the partition's id and its own `drivers` list — classed by their manifests under the kernel's driver policy. A driver whose manifest does not list the partition, or that the partition does not list, is invisible to its harness |
| Crossing tools | `partition_memory_search` (a read) and `partition_memory_remember` (a write) reach another partition's facts; `partition_crossing_tool` offers the coordinator another partition's driver or session tool as `<partition>__<tool>` |
| Policy | The partition's `PartitionPolicy` composed with the operator policy through `StrictestPolicy`, so it never loosens the harness policy (K-I6) |

> **Note:** `Harness::with_runner` takes a runner you built. Give it the partition's identity
> source yourself: `Runner::builder().identity_source(partition.identity_source("adk-rust", "harness"))`.

## Partitions under `adk-rust up`

`[[kernel.partitions]]` in the user config makes `adk-rust up` run one harness per partition,
and `adk-rust partitions` lists and changes the table; see
[kernel commands](cli.md#partitions).

```toml
[kernel]
coordinator = "home"

[[kernel.partitions]]
id = "home"
cross_partition = "coordinator_with_approval"
quiet_hours = "22:00-07:00"

[[kernel.partitions]]
id = "work"
mode = "auto"
cross_partition = "coordinator_with_approval"
quotas = { max_running = 2, daily_spend_cap_usd = 1.5 }
```

With long-term memory and consolidation on, each partition in Ask or Auto gets a consolidation
tick of its own, which distils only the sessions its partition's harness ran. Each partition's
harness gets the mounted drivers its `drivers` list names, and is the audience of its own
partition's `/proc` observations. The coordinator's harness also reaches every other partition's
drivers, each call a [crossing](#crossing-into-another-partitions-drivers).

## Hosting partitions on a kernel

`Harness::kernel` on a partition's harness registers its runner as the targets
`<app>@<partition>` and `harness.goal@<partition>`, bound to the partition, and its progress
source as the partition's (`KernelBuilder::partition_progress_source`); `host_partition` adds
another partition's harness to the same builder the same way:

```rust
use std::sync::Arc;

use adk_rust::harness::Harness;
use adk_rust::kernel::{InMemoryProcessStore, Kernel};

fn kernel(
    home: Harness,
    work: Harness,
    runs: Arc<dyn adk_rust::DurableRunStore>,
) -> adk_rust::kernel::Result<Kernel> {
    let home = home.with_run_store(runs.clone());
    let work = work.with_run_store(runs);
    work.host_partition(home.kernel())
        .process_store(Arc::new(InMemoryProcessStore::new()))
        .build()
}
```

| Binding | Effect |
|---|---|
| A spawn of `adk-rust@work` without a partition | Runs in `work` |
| A spawn of `adk-rust@work` in `home` | Refused with `kernel.config` |
| A run of `adk-rust@work` enqueued outside the kernel (`/api/runs`) | Listed in `work` by `ps` |
| A process of `work` continuing a session a `home` agent ran | Refused with `kernel.config` |

The kernel reads a process's goal progress, loop-guard verdict and usage from its partition's
progress source — by the process's own partition — and hands every source the process table and
the [ledger](proc.md) at start, so each partition's harness attributes its own spend, parks its own
stalled goals and writes its own ledger rows. A partition with no source of its own falls back to
the main one (`KernelBuilder::progress_source`); its processes keep their partition. Two sources
for one partition are refused at `build`, and a source for a partition the table does not hold is
warned about at start.

## Self-observation per partition

| Part | Per partition |
|---|---|
| Ledger rows | A partition's harness writes its rows — intents, tool calls, its audit records — in its partition, never `default`; the kernel's own rows carry the process's partition |
| Observations | `KernelBuilder::observation_audience(partition, ..)` registers each partition's audience; an observation is put only to its own partition's audience, and `adk-rust up` registers each partition's harness for its partition |
| A confirmed observation | `Harness::observation_audience` of a partition's harness remembers it as a `Known` fact in the partition's memory scope, not user-wide; it answers no one for another partition's observations and refuses to confirm one, so a partition's observation never reaches another partition's facts |
## Crossings

The partition's `PartitionPolicy` decides the calls of its crossing tools and lets every other
call through to the harness policy:

| Call | Decision |
|---|---|
| A crossing tool reaching the agent's own partition | `Allow` |
| Any agent but the coordinator reaching another partition | `Deny` |
| The coordinator, into or out of a partition with `crossPartition: deny`, into an unknown partition, or into a partition its own partition blocks | `Deny` |
| The coordinator's read whose crossing fingerprint was approved earlier in the same run | `Allow` |
| Any other crossing by the coordinator | `RequireApproval`, naming the crossing and its answers |

The crossing tools also check the partition table before they act, so even a harness built
without the policy never crosses for an agent other than the coordinator.

### The unit of approval: the crossing fingerprint within one run

A crossing's fingerprint is the call's `ToolConfirmationRequest::fingerprint()` — the acting agent,
the tool's origin and declaration, the tool name and the canonical arguments — extended with the
target partition and the resource class (`memory`, `driver` or `session`). An approved **read**
lets the coordinator's identical reads through for the rest of that run: the same agent, the same
tool, the same arguments, the same target. A run is the call's durable run, so the approval holds
across the invocations that resume it; outside a durable run the invocation is the run.

| Crossing | Approval |
|---|---|
| A read | Once per run, per crossing fingerprint |
| A write, a driver call of effect class `Financial` or `PublishPublic`, any other change | Every call |
| The next run | Asks again |

No standing rule and no expiring rule answers a crossing: the harness offers no rule at the
prompt, a standing rule only answers the harness's own approval and never another layer's, and a
run-wide `tool_approvals` entry naming a crossing tool is removed when the run starts. Auto mode
asks too.

### Crossing into another partition's drivers

The coordinator reaches another partition's drivers only as crossing tools:
`HarnessAgentBuilder::partition_driver_crossings(&drivers, partitions)` offers, for each partition
named, the tools that partition's own harness is served — its manifest lists the partition and
the partition's `drivers` names it — each as `<partition>__<tool>`, such as `work__read_note`
(`DriverSupervisor::crossing_toolset`). `adk-rust up` gives the coordinator every other
partition's drivers this way, and a managed definition's coordinator its other partitions'.

| Rule | How |
|---|---|
| Read from the table at every listing | A partition that takes no crossing (`crossPartition: deny`), the coordinator's own when it takes none, and one the coordinator's partition blocks offer nothing; a driver mounted later appears at the next listing |
| Classed as the tool it reaches | The crossing is classed by the driver's manifest and governed by the driver policy as the wrapped tool, so a stale tool and the classes `publish_public` and `financial` are asked every call (K-I9, K-I14) |
| A read once per run | Only a tool the manifest classes `read`, with the approved declaration, is offered as a read, approved once per crossing fingerprint and run; every other class is approved one call at a time |
| Never waived | A run-wide `tool_approvals` entry for any tool named `<partition>__…` of a partition the coordinator crosses into is removed when the run starts; no standing rule answers one |
| Only the coordinator | `build` fails when the harness is not its partition's coordinator |

```rust
use std::sync::Arc;

use adk_harness::HarnessAgent;
use adk_kernel::drivers::DriverSupervisor;
use adk_kernel::{Partition, Partitions};

fn coordinator(
    model: Arc<dyn adk_core::Llm>,
    workspace: adk_devtools::Workspace,
    home: Partition,
    table: Partitions,
    drivers: &DriverSupervisor,
) -> adk_core::Result<HarnessAgent> {
    HarnessAgent::builder("harness")
        .model(model)
        .workspace(workspace)
        .partition(home, table)
        .partition_coordinator()
        // Home's own drivers as tools, work's as crossings.
        .partition_drivers(drivers)
        .partition_driver_crossings(drivers, ["work"])
        .build()
}
```

### The prompt

A crossing is put to the person with the crossing named and three answers:

| Answer | Effect |
|---|---|
| "allow this read of *work › calendar* for this run" ("allow this change to *X* once" for a write) | The call runs |
| "deny" | The call does not run; the rest of the turn's calls are denied |
| "deny and block crossings to *work* from this partition" | The call does not run, and `work` joins the partition's `blockedCrossings`: every later crossing into it is denied at the policy without asking |

`ApprovalPrompter::prompt_crossing` receives a `CrossingPrompt` — the rendered call, the
`Crossing`, the three answers — and returns a `CrossingAnswer`. A prompter that does not
implement it is asked through `prompt` with a title naming the crossing: an approval allows it, a
denial denies it, a denial with a remembered rule blocks the partition, and an approval with other
arguments is refused, since a crossing runs as asked or not at all. Under a kernel without a
terminal, the crossing waits in the approvals inbox with the same answers in its reason; a
decision wakes the process.

### Recurring crossings

A crossing that keeps happening says the resource sits in the wrong partition.
`Partitions::recurring_crossings(since, at_least)` counts each request once — held, denied, or
let through by its run's approval — by source, target, resource and name, and phrases the
remedy as partition membership: move the resource, or merge the partitions, never a waiver
("partition home crossed into work › calendar 14 times; if it needs it this often, move
calendar into home, or merge home and work").

The kernel's daily review ([proactivity](proactivity.md#the-daily-review)) lists, in the report
of the partition the requests come from, each crossing that recurs at least
`ReviewReport::CROSSING_THRESHOLD` (5) times in `ReviewReport::CROSSING_WINDOW` (7 days), with
its remedy (`ReviewReport::crossing_remedies`). The review changes no partition and waives no
crossing.

## Audit records

| Action | When | Fields |
|---|---|---|
| `kernel.partition.created` | A partition is added | `partition`, `fields`, `from`, `to` |
| `kernel.partition.changed` | Its mode, memory scope, drivers, crossing setting or blocks change | `partition`, `fields` (the changed fields), `from`, `to` |
| `kernel.partition.crossing` | Every crossing: denied, held, approved, or let through by the run's approval | `partition`, `to`, `resource`, `name`, `access`, `outcome` (`denied`, `held`, `approved`, `runGrant`), `run`, `tool`, `fingerprintDigest`, `reason`, `by` |

A crossing record holds a SHA-256 digest of the crossing fingerprint, never the call's arguments.
The same entries are kept in the partition store's crossing ledger
(`Partitions::crossings(CrossingFilter)`), which the run's approvals are read from.

## Stores

| Backend | Type | Feature of `adk-kernel` |
|---|---|---|
| In memory | `InMemoryPartitionStore` | — |
| SQLite | `SqlitePartitionStore`: `adk_kernel_partitions` and `adk_kernel_crossings` on the `harness.db` pool, migrations in `_adk_kernel_partition_migrations` | `sqlite` (default) |
| PostgreSQL | `PostgresPartitionStore`, migrating under an advisory lock | `postgres` |
| Redis | `RedisPartitionStore`, under the `{adk:kernel}` hash tag beside the process store | `redis` |
| MongoDB (Alpha) | `MongoPartitionStore`: `adk_kernel_partitions`, `adk_kernel_crossings` and `adk_kernel_crossing_grants` in the database the run store uses | `mongodb` |
| Firestore (Alpha) | `FirestorePartitionStore`: the same three collections, queried without composite indexes | `firestore` |
| Neo4j (Alpha) | `Neo4jPartitionStore`: `AdkKernelPartition`, `AdkKernelCrossing` and `AdkKernelCrossingGrant` nodes | `neo4j` |

A new backend passes `partition_store_conformance_tests!` (feature `conformance`):

```toml
[dev-dependencies]
adk-kernel = { version = "3.0.0", features = ["conformance"] }
async-trait = "0.1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

```rust
use std::sync::Arc;

use adk_kernel::conformance::PartitionStoreHarness;
use adk_kernel::{InMemoryPartitionStore, PartitionStore};

struct InMemory;

#[async_trait::async_trait]
impl PartitionStoreHarness for InMemory {
    async fn open(&self) -> Arc<dyn PartitionStore> {
        Arc::new(InMemoryPartitionStore::new())
    }

    fn is_durable(&self) -> bool {
        false
    }
}

adk_kernel::partition_store_conformance_tests! { harness = InMemory }
```

## Partitions in the managed form

A [managed](managed.md) definition declares partitions and their coordinator in its kernel block,
as `[[kernel.partitions]]` and `kernel.coordinator` do in `harness.toml`:

```json
{
  "partitions": [
    { "id": "home", "class": "home-agents", "crossPartition": "coordinator_with_approval" },
    { "id": "work", "class": "work-agents", "defaultMode": "auto", "crossPartition": "coordinator_with_approval" }
  ],
  "coordinator": "home",
  "wakeSets": [
    { "name": "standup", "anyOf": [{ "webhook": { "hook": "standup" } }], "input": "Prepare the standup.", "partition": "work" }
  ]
}
```

| Part | On the tenant's kernel |
|---|---|
| Partition table | The tenant's own (`TenantKernelBuilder::partition_store`, in memory by default): each partition with the tenant as its tenant; `ManagedKernels` refuses a tenant whose partition store another tenant holds |
| Agents | `TenantKernelBuilder::partition_resolver` builds one agent per partition — `adk-harness`'s `HarnessResolver` builds one harness per partition with `partition` and, for the coordinator, `partition_coordinator` — as `<definition>@<partition>`, bound to its partition and acting with its class and the tenant |
| The definition's own target | The coordinator's agent, else the first partition's by id; the runtime's sessions run it |
| Wake sets | A wake set in a partition starts that partition's agent |
| Drivers | With `TenantKernelBuilder::drivers`, the tenant's own supervisor: a driver a partition mounts is proposed in the tenant's inbox and mounted when a person approves it; each partition's harness mounts the drivers its `drivers` names, and the coordinator's reaches the other partitions' drivers as crossings ([managed form](managed.md#a-definitions-drivers)) |
| Refused | A coordinator the block does not declare, a partition declared twice, a partition key the record does not have (`tenant` included), a partition another hosted definition of the tenant declares |

Without a partition resolver the partitions are recorded and the definition's agent runs in the
default partition.

```toml
[dependencies]
adk-kernel = { version = "3.0.0", features = ["managed"] }
adk-harness = { version = "3.0.0", features = ["managed", "kernel"] }
adk-managed = "3.0.0"
adk-session = "3.0.0"
```

```rust
use std::sync::Arc;

use adk_harness::packs::managed::HarnessResolver;
use adk_kernel::managed::TenantKernel;
use adk_managed::ManagedStores;
use adk_session::InMemorySessionService;

fn tenant() -> adk_kernel::Result<TenantKernel> {
    let harnesses = Arc::new(HarnessResolver::new("./workspaces/tenant-a"));
    TenantKernel::builder(
        "tenant-a",
        ManagedStores::in_memory(),
        Arc::new(InMemorySessionService::new()),
    )
    .resolver(harnesses.clone())
    .partition_resolver(harnesses)
    .build()
}
```

## The control plane

| Route | Purpose | `operator` | Recorded |
|---|---|---|---|
| `GET /api/kernel/partitions` | The partition table | — | — |
| `POST /api/kernel/partitions` | Create or replace a partition; body: the partition record | Yes | `partition.put` |
| `POST /api/kernel/partitions/{id}/mode` | Set a partition's mode; body `{"mode": "auto"}` | Yes | `partition.mode` |

`ControlClient::{partitions, put_partition, set_partition_mode}` call them.

## Limits

| Limit | Detail |
|---|---|
| Global memory entries | `Partition::scope_memory` searches through the memory's project scoping, which also returns entries written with no project. A partition writes none; entries another writer stored globally stay visible |
| A session no partition's harness has run | The first partition harness to run it marks it as its own, so a session an unpartitioned harness ran before partitions existed joins whichever partition continues it first |

## Read next

- [The agent kernel](index.md): processes, freezing and using the kernel from code
- [The control plane](control-plane.md): transports, authentication and the other routes
