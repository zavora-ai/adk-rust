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
| `proactivity` | The tenant kernel's `ProactivityPolicy`: open proposals, the daily proactive budget, quiet hours and how proposals are approved. One kernel has one policy, so a second definition may repeat it or leave it out |
| `proactivity.consolidation` | The only switch for the consolidation tick: it is off in the managed form unless a definition sets it (§13 Q1), and runs where the operator registered a consolidator |
| `drivers` | `DriverManifest`s, validated and recorded; no driver is mounted without a person's approval |
| `partitions` | The partition record's plain data, validated and recorded; the definition's agent runs in the default partition |

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
| Control plane | None of its own; mount `adk_kernel::control::router(kernel)` on the host's server |

A definition hosted after `TenantKernel::start`, or hosted again changed, restarts the tenant's
kernel over the same stores: its running processes park at their next model-call boundary and
the new kernel resumes them. The same definition hosted again changes nothing.

`ManagedKernels` holds a host's tenants, one kernel each, and refuses a tenant whose run store,
decision store, state store, approval service or process store another tenant holds: a shared
run store would let one tenant's workers claim the other's runs. Distinct store values over one
database are not detected, so give each tenant its own database, schema or key namespace.

## What a tenant's definition cannot do

A definition comes from a tenant, so its kernel block cannot widen what the operator allows.

| Limit | Refused, naming the key |
|---|---|
| The agent | A definition with a kernel block runs only an agent the operator's resolver builds, such as a harness definition under `HarnessResolver`; a plain definition stays in the session loop |
| Drivers | `sandbox: "unsandboxed"`; pinned `declarationDigests`; a credential-named variable with a plain value instead of a `secret://` reference; a partition the block does not declare |
| Wake sets | The `interactive` queue a person's turn holds; a `fileChange` wake on the host's files; approval, run-finished, time, budget and manual wakes, which name one sleeping process; an unparsable cron expression |
| Partitions | A `tenant` key: the tenant is the hosting kernel's |
| Consolidation | No field lets an agenda item of origin `Consolidation` start without a person's approval, in any mode (K-I23) |

## The managed approval path

| Step | What happens |
|---|---|
| 1. A wake | A webhook delivery (served with `webhook_wake_hook` on the tenant's run store), a queue message or a cron tick becomes a keyed `kernel.wake` run that the maintenance worker routes to the wake set's trigger |
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
| `adk-kernel/tests/managed_def_fixtures.rs`, `tests/fixtures/managed/` | Twelve golden fixtures: each block's canonical form with its defaults explicit, the consolidation switch, the triggers a tenant kernel registers, and every refusal's message |
| `adk-kernel/tests/managed_tenants.rs` | A definition with its block round-trips through the managed state store unchanged and is hosted again on a rebuilt session; a cron wake for tenant A runs on A's stores only and nothing reaches tenant B; a held call runs once under the action ledger after the tenant's approval; a definition hosted after start restarts the kernel; one policy per tenant, the tick off unless a definition turns it on; a consolidation item waits for a person in Auto; no tenant kernel opens a control plane |
| `adk-kernel/tests/managed_tenant_property_tests.rs` | Over random deliveries, approvals and approvals attempted through another tenant's service, across two or three tenants sharing a session service: no wake, process, approval or session crosses tenants (`proptest`, 100 cases) |
| `adk-harness/tests/managed_kernel_tests.rs` | A harness definition on a tenant kernel: the held write runs once after the tenant's approval, and the harness's follow-up lands on the tenant's agenda |
| `adk-harness/tests/voice_kernel_tests.rs` | A scripted spoken yes wakes a parked process; a freeze-parked process is refused and wakes only on unfreeze |
| `examples/harness_kernel` `m1_one_kernel_per_tenant`, `v1_voice_answers_a_parked_process`, `live_managed_tenant_approval`, `live_voice_parked_process` | The managed form with two tenants scripted and one tenant live on `claude-sonnet-5-5` and `gemini-3.5-flash-lite`; a parked process decided by voice, scripted and live on Gemini Live |

## Related

- [The agent kernel](index.md)
- [Drivers](drivers.md)
- [Serving the harness on the managed runtime](../harness/serving.md#on-the-managed-runtime)
- [Harness voice](../harness/voice.md)
- Spec: `docs/design/spec-3.2-agent-kernel.md` §7.11, §7.13, §10 M1, §13 Q1
