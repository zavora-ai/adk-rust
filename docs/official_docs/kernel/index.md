# The Agent Kernel

The Agent Kernel is the always-on form of the [harness](../harness/index.md): a resident
supervisor that keeps the harness's work as durable processes, wakes them on time, file
changes, webhooks, queues, approvals and finished runs, and lets every surface — the
terminal, a browser, a phone — attach to the same running state (spec 3.2). `adk-rust up`
starts it; the `adk-kernel` crate holds it.

```bash
adk-rust up --mode ask --dir ./project          # the kernel, in the foreground
adk-rust ps                                     # its processes, from another terminal
adk-rust attach                                 # a REPL whose turns the kernel runs
adk-rust goal "make the tests pass" --until "cargo test"   # enqueued on the kernel
adk-rust freeze --reason "incident 12"          # every process parks; nothing starts
```

Three rules shape it:

| Rule | Consequence |
|---|---|
| **Always resumable, not always running** | A sleeping process is a `Waiting` durable run with a wake set and no lease: no model calls, no CPU |
| **Every wake is a governed run** | A wake enqueues a run or wakes a parked one; the run passes modes, standing rules, guardrails, confirmation, the action ledger and audit like an interactive turn |
| **The kernel owns state, surfaces attach** | The CLI, the web and phones are terminals on one control plane; closing a laptop changes nothing |

## What `adk-rust up` runs

`up` builds what [`adk-rust serve`](../harness/serving.md) builds — the approval inbox with
`operator` routing, background runs with the harness targets, the request extractor and its
non-loopback refusal, the `--store` backend — and adds the kernel:

| Part | What it is |
|---|---|
| Workers | One `DurableWorker` per queue class: `interactive` (polls every 500 ms), `background` (2 s), `maintenance` (5 s); each parks a run a freeze refuses instead of failing it |
| Scheduler | `DurableScheduler` over the schedule store, ticking every second |
| Process table | The durable runs plus the kernel's side table: parent, partition, queue, kind, wake set, suspension |
| Event bus | Every process change, wake, hold, decision and governance change, numbered by cursor |
| Control plane | `/api/kernel/...` on the Unix socket at the kernel home (a named pipe on Windows) and on HTTP, beside the approvals inbox |
| Kill switch | `GovernanceControl` mirrored from the store every 250 ms, so a freeze reaches every replica within a second |
| Audit | One signed log per replica (`kernel-<id>`), holding the harness's tool-call records and the kernel's `kernel.*` records |
| Activity ledger | Content-free rows of what the agents did, beside the other stores, and the daily baseline pass ([`/proc`](proc.md)) |

| Default | Value |
|---|---|
| Kernel home | `<config dir>/adk-rust`: `kernel.sock`, `harness.db`, `kernel-id`, `kernel-audit.key`, `ledger.key` |
| HTTP | `127.0.0.1:8080` (`--bind`); anything but loopback needs `--auth-token` |
| Mode | `plan`, as `serve`; `--mode ask` lets the agent edit and run commands after approval |
| Stores | `harness.db`, shared with the REPL and `serve`; `--store postgres://…` or `redis://…` for replicas ([backends](backends.md)) |
| Shutdown | Ctrl-C, `SIGTERM` or a Windows service stop: the HTTP server stops accepting, the kernel drains for 30 s and stops |
| Service | `adk-rust up --install` runs it as a launchd agent, a systemd user unit or a Windows service, as you ([service installation](service.md)) |

Nothing is lost on a hard kill: a run's lease expires and the next claim resumes it from its
checkpoint.

## Processes

A process is a durable run (spec 3.2 §7.3). Its pid is the run id and its state is the run's:

| State | Meaning |
|---|---|
| `queued` | Waiting for its queue's worker |
| `running` | Claimed under a lease |
| `waiting` | Asleep on a wake set, parked on a held call, or suspended; holds no lease |
| `completed`, `failed`, `cancelled` | Finished |

Each process carries a live view — age, spend today and over seven days, burn rate and the
projection to the end of the day for a running process, model and tool calls, the loop
guard's last verdict and a goal's progress — that `adk-rust ps` and `top` show. A process
suspended by a freeze shows `suspended: freeze` and resumes only on `unfreeze`; a manual
suspension resumes only on `adk-rust resume`.

## Freezing

`adk-rust freeze`, `POST /api/kernel/freeze` and `POST /api/admin/freeze` all call
`Kernel::freeze`: the freeze is written to the governance store, every replica's workers park
their running processes `Waiting` at the next model-call boundary, and nothing starts until
`unfreeze`. A call refused mid-turn is answered `not run` and the action ledger keeps completed
calls from running twice. `unfreeze` wakes exactly the processes the freeze parked.

## Using the kernel from code

`adk-kernel` hosts any `Arc<Runner>` and depends on no harness type. The harness's `kernel`
feature returns a builder with the harness registered:

```toml
[dependencies]
adk-rust = { version = "3.0.0", features = ["harness-kernel"] }
```

```rust
use std::sync::Arc;

use adk_rust::kernel::{InMemoryProcessStore, Kernel, KernelConfig};

async fn run(runner: Arc<adk_rust::runner::Runner>) -> adk_rust::kernel::Result<()> {
    let runs = Arc::new(adk_rust::InMemoryRunStore::new());
    let kernel = Kernel::builder()
        .config(KernelConfig::default().with_app_name("ops"))
        .run_store(runs)
        .process_store(Arc::new(InMemoryProcessStore::new()))
        .agent("ops", runner)
        .build()?;
    kernel.start().await?;
    kernel.shutdown_on_signal().await
}
```

| Feature of `adk-kernel` | Default | Adds |
|---|---|---|
| `sqlite` | on | `SqliteProcessStore` on the `harness.db` pool |
| `postgres`, `redis` | off | The kernel's stores in the backend replicas share |
| `mongodb`, `firestore`, `neo4j` | off | **Alpha**: the kernel's stores on MongoDB, Firestore or Neo4j, with the same backend of `adk-session`'s stores ([backends](backends.md)) |
| `webhooks` | on | Webhook wake sources over `WebhookIngress` |
| `proc` | on | The content-free activity ledger and its stores; the live aggregates' OTel GenAI histograms |
| `proc-baselines` | off | The daily baselines and observations ([`/proc`](proc.md)) |
| `service` | on | `adk_kernel::service`: launchd, systemd and Windows service installation |
| `conformance` | off | `process_store_conformance_tests!` and `partition_store_conformance_tests!` for a new store backend |
| `managed` | off | The managed form: `KernelDef` in a managed definition and one kernel per tenant ([managed form](managed.md)) |
| `channels` | off | The chat channel contract: `ChannelSink`, the channel gateway, the binding store on every backend, the adapter contract suite ([pairing](pairing.md)) |

## Read next

- [Kernel commands](cli.md): `up`, `attach`, `ps`, `top`, `kill`, `suspend`, `resume`,
  `freeze`, `unfreeze`, `events`, and `goal` on a running kernel
- [The control plane](control-plane.md): transports, authentication, routes and audit records
- [Pairing devices and chat channels](pairing.md): `adk-rust pair`, scoped device tokens, and
  the chat channel contract
- [Running the kernel as a service](service.md): `up --install`, `--uninstall` and `--print`
  on macOS, Linux and Windows
- [The managed form](managed.md): one kernel per tenant on the managed runtime
- [Self-observation: `/proc`](proc.md): the content-free activity ledger, baselines and
  observations
- [Kernel store backends](backends.md): every kernel store on in-memory, SQLite, PostgreSQL
  and Redis, and all but the partition store on MongoDB, Firestore and Neo4j (Alpha)
- [Partitions](partitions.md): one harness per partition, memory scoping, and crossings by the
  coordinator under approval
