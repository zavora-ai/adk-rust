# Drivers

A driver is an MCP server the [agent kernel](index.md) runs as a supervised child process
under a manifest you approved (spec 3.2 §7.7). The kernel starts it in the OS sandbox,
hands it its secrets through the credential broker, restarts it when it crashes, and
notices when it changes what its tools declare. Its tools reach the harness classed by
the manifest, never by the server's own hints.

## The manifest

```toml
# email.toml: mount it with `adk-rust drivers mount email.toml`; a workspace's
# .adk/drivers/email.toml may only restrict it
id          = "email"
command     = "mcp-email"
args        = ["--imap"]
env         = { EMAIL_TOKEN = "secret://email/token", LOG_LEVEL = "info" }
partitions  = ["work"]
lifecycle   = "lazy"                       # lazy | eager
sandbox     = "os"                         # os | container | unsandboxed
restart     = { max_attempts = 5, initial_delay_ms = 500, max_delay_ms = 30000, backoff_multiplier = 2.0 }

[tools]                                    # every other tool is `other`
list_inbox   = "read"
get_email    = "read"
create_draft = "write_local"
send_draft   = "send_external"
```

| Key | Default | Meaning |
|---|---|---|
| `id` | — | The driver and MCP server id: ASCII letters, digits, `-`, `_` |
| `command`, `args` | — | What runs |
| `env` | `{}` | The server's environment; a `secret://` value is resolved through the credential broker (below), and a plain value under a credential's name (`*_TOKEN`, `*_SECRET`, `*_PASSWORD`, `*_API_KEY`, …) is refused |
| `partitions` | `["default"]` | The partitions whose agents may use it |
| `lifecycle` | `lazy` | `lazy` starts it when a partition's agent first lists its tools; `eager` at mount and kernel start |
| `protocol` | `initialize` | The MCP handshake: `initialize`, `auto` or `discover` (2026-07-28) |
| `sandbox` | `os` | `os` runs it under Seatbelt (macOS) or bubblewrap (Linux); `container` has no driver launcher yet and is treated as an unavailable sandbox; `unsandboxed` runs it on the host, recorded as such |
| `allow_read`, `allow_write` | `[]` | Paths a sandboxed server may read, or read and write, besides its program's directory, its scratch home and its secrets |
| `allow_network` | `true` | Whether a sandboxed server may use the network |
| `restart` | `RestartPolicy::default()` | Restarts after a crash: attempts and backoff |
| `sampling` | `false` | Whether the server may sample the harness model, metered against the run that called it |
| `data_classes` | `[]` | Labels of the data it handles, shown when you approve it |
| `[tools]` | — | The effect class of each tool |

Every driver declares MCP tasks, so a server that needs a long wait can hand back a durable
task handle.

### Effect classes

| Class | Harness class | ReadOnly / Plan | Ask / Auto |
|---|---|---|---|
| `read` | trusted read-only `External` | Allow | Allow |
| `write_local`, `schedule_with_others`, `send_external`, `delete`, `other` | trusted `External` with effects | Deny | Ask; a standing rule may allow |
| `publish_public`, `financial` | trusted `External` with effects | Deny | Ask on every call: no standing rule or mode allows it (K-I9) |

A server's `readOnlyHint` never moves a tool between rows (spec 3.1 I12). A `read` tool is
never retried as a write is and is not recorded in the action ledger; every other class is
`NonIdempotent`.

### A workspace may only restrict

A manifest in a workspace's `.adk/drivers/` with the id of one of yours may narrow the
partitions, the sandbox access and sampling, require a sandbox you left off, move an eager
driver to lazy, and raise a tool to a stricter class. It may not change the command, its
arguments, its environment, the handshake or the restart policy, and it cannot add a driver
you did not declare.

```rust
use adk_kernel::drivers::load_manifests;

let manifests = load_manifests("/home/me/.config/adk-rust/drivers", Some(".adk/drivers".as_ref()))?;
# Ok::<(), adk_kernel::KernelError>(())
```

## Mounting is approving

Nothing runs before you mount a manifest (K-I13). Mounting shows what you are approving:

```text
$ adk-rust drivers mount email.toml
driver email
  command     mcp-email --imap
  partitions  work
  sandbox     os
  network     allowed
  secret      EMAIL_TOKEN ← secret://email/token (through the credential broker)
  tools (any other tool is `other`):
    create_draft             write_local
    get_email                read
    list_inbox               read
    send_draft               send_external
Mount this driver? [y/N] y
mounted email            stopped          [work] os (not started)  mcp-email --imap
```

The approval is recorded as `kernel.driver.mounted` with you as the actor, the command's
digest, the partitions and the classes. The first time the driver lists its tools after
approval, the kernel pins each tool's declaration digest.

| Command | Does |
|---|---|
| `adk-rust drivers list` | Each driver's state, partitions, isolation and command; tools changed since approval |
| `adk-rust drivers mount FILE [--yes]` | Shows the manifest, restricted by the workspace's, and mounts it once you approve |
| `adk-rust drivers unmount ID` | Stops it and forgets its manifest and parked calls |
| `adk-rust drivers restart ID` | Restarts it, minting its secrets again |
| `adk-rust drivers reapprove ID` | Approves its changed declarations; your standing rules follow them |

`--home DIR`, or `--server URL` with `--token` for a kernel over HTTP, goes before the
subcommand: `adk-rust drivers --home ~/.adk mount email.toml`.

### Drivers in the user config

`[kernel] drivers` in `harness.toml` lists manifests — files, directories of them, or inline
tables — that `adk-rust up` mounts when the kernel starts ([`[kernel] drivers`](cli.md#drivers)).
Each one still needs your approval: `up` shows the same proposal and asks before it mounts a
driver that is not mounted exactly as written, and mounts nothing new without a terminal. A
driver already mounted as written keeps its approval and its pinned declarations across
restarts. From code, `DriverSupervisor::is_mounted_as(&manifest)` tells whether a manifest
needs a new approval, and `DriverSupervisor::mount_at_start(manifest, by)` mounts an approved
one when `Kernel::start` runs, on the kernel's audit trail and event bus.

## The sandbox and the environment

A driver never inherits the kernel's environment. It gets `PATH`, the locale and terminal
variables, `HOME`, `TMPDIR`, `USER` and `SHELL` (and the Windows equivalents), the plain
values of its manifest's `env`, and a file variable per secret. Under `sandbox = "os"` its
`HOME` and `TMPDIR` are a scratch directory at `<kernel home>/drivers/run/<id>/home`, the
only place it may write besides `allow_write`.

The sandbox runs the file the command resolves to, through any link, and opens read-only
what that program needs to start, outside the system directories every sandbox reads:

| Program | Opened read-only |
|---|---|
| Any | Its directory |
| A script | Its `#!` interpreter's directory, and for `#!/usr/bin/env NAME`, the directory of the `NAME` that `PATH` finds |
| An ELF program | Its dynamic loader's directory and its `RUNPATH` or `RPATH` directories; a loader or library in `/nix/store` opens the store |

The directories are recorded on the driver's isolation (`programPaths`) and in its
`kernel.driver.started` record. A server that reads files beyond them, such as a package
tree next to its entry point, lists them in `allow_read`.

When no enforcer runs on the host — a Linux host without bubblewrap, for example — the
driver runs unsandboxed and `drivers list` says why. `KernelConfig::fail_if_sandbox_unavailable` refuses
to start it instead.

On Windows every driver, sandboxed or not, runs in a Job Object of its own (spec 3.2 §10
W1): it starts suspended and runs only once it is in the job, so every process it starts
belongs to the job too. Stopping, restarting or unmounting the driver, a crash, and the
kernel's own exit end the whole tree. The kernel records it on the driver as
`processTree: "jobObject"` (`DriverRecord::process_tree`), in its `kernel.driver.started`
record, and `drivers list` adds `in a job object` to the driver's isolation. On macOS and
Linux `processTree` is absent and ending a driver ends its own process only.

## Secrets

An `env` value `secret://<scope>/<name>` is resolved through the kernel's credential broker
at every start, as the driver's own agent, `urn:adk:agent:<app>/driver.<id>`. The secret is
written to `<kernel home>/drivers/run/<id>/secrets/<NAME>`, readable only by you, and the
server gets `<NAME>_FILE` naming that file. `<NAME>` itself is not set, so the secret never
appears in the server's environment, its children's, or a process listing. The directory is
removed when the driver stops.

```rust
use std::sync::Arc;
use adk_auth::SecretCredentialBroker;
use adk_core::AgentId;
use adk_kernel::drivers::DriverSupervisor;
use adk_kernel::stores::InMemoryDriverStore;

# fn example(vault: Arc<dyn adk_auth::secrets::SecretProvider>) -> adk_core::Result<()> {
let broker = SecretCredentialBroker::new(vault).bind(
    AgentId::new("urn:adk:agent:adk-rust/driver.email")?,
    url::Url::parse("secret://email/token").expect("a secret reference"),
    "email-imap-token",
    vec![],
);
let drivers = DriverSupervisor::builder(Arc::new(InMemoryDriverStore::new()))
    .credential_broker(Arc::new(broker))
    .build();
# Ok(())
# }
```

## Supervision and events

Each driver runs on an `McpServerManager` of its own. Its status changes reach the event bus
as `driver` events and the audit log as `kernel.driver.*` records:

| Change | Event `state` | Record |
|---|---|---|
| The server starts the first time | `starting`, `running` | `kernel.driver.started` |
| Its process exits or its connection closes | `crashed` | `kernel.driver.crashed` |
| A restart under the manifest's backoff | `restarting` (`restart attempt N`), `starting`, `running` | `kernel.driver.restarted` |
| The restart policy's attempts run out | `failedToStart` | — |
| A tool declares something other than its pin | `needsReapproval` (the tools that changed) | `kernel.driver.stale` |
| Re-approval | `running` (`re-approved by …`) | `kernel.driver.reapproved` |
| Mount, unmount | `stopped` | `kernel.driver.mounted`, `kernel.driver.unmounted` |

```bash
adk-rust events --follow
```

## When a tool changes (K-I14)

A tool whose declaration — description, parameters or annotations — differs from the one
pinned at approval is stale, and so is a tool that was not there at approval:

- the harness's standing rules pinned to its old declaration stop matching, recorded as
  `harness.rule.stale`;
- the kernel's driver policy asks before every call of it, whatever the mode;
- the driver is `needsReapproval`.

`adk-rust drivers reapprove ID` pins what the tools declare now and moves your standing
rules for the changed tools onto their new declarations. Other people's rules stay pinned
to what they approved.

## Questions a driver asks (spec 3.2 §13 Q9)

A server on the 2026-07-28 revision asks for input by ending the call with
`input_required`; an older one sends `elicitation/create` while the call runs. Either way a
driver's call parks:

1. The kernel stores the parked call — server, tool, arguments, call id, the server's
   `requestState`, its questions and their digest, and how it resumes (`mrtr`, `task` or
   `legacy`) — beside the question's approval, as the run's checkpoint.
2. The question goes to the run's confirmation handler, or to the approvals inbox as
   `driver:<id>/elicitation`, and the agent holds its call; a durable run parks `Waiting`
   and holds no lease.
3. The answer — in this process, or after the kernel restarted — wakes the run. The agent
   dispatches the held call again and the driver re-issues it: a new `tools/call` with the
   answers and the stored `requestState`, `tasks/get` and `tasks/update` for a task, and for
   a legacy call a second run only when the tool is retry-safe, otherwise "not run".

In the inbox, approve to accept a form with its schema's defaults, edit to answer it — the
edited arguments are the form's content, keyed by question when there are several — or
reject to decline. The re-issued call is the call's first execution on the server, and the
action ledger keeps a resumed call from executing twice.

When a call needed your approval before its server asked, that approval carries to the call
that resumes, once, until the question lapses: you answer the question and are not asked to
approve the same call again. A denial by the policy in force when it resumes still wins.

## Using drivers from code

```rust
use std::sync::Arc;
use adk_harness::HarnessAgent;
use adk_kernel::drivers::{DriverManifest, DriverSupervisor};
use adk_kernel::stores::InMemoryDriverStore;
use adk_kernel::{Kernel, PartitionId};

# async fn example(
#     runs: Arc<dyn adk_core::DurableRunStore>,
#     model: Arc<dyn adk_core::Llm>,
#     workspace: adk_devtools::Workspace,
# ) -> anyhow::Result<()> {
let drivers = DriverSupervisor::builder(Arc::new(InMemoryDriverStore::new())).build();
let agent = HarnessAgent::builder("work")
    .model(model)
    .workspace(workspace)
    // The tools of the drivers mounted for this partition, classed by their manifests,
    // under the kernel's driver policy.
    .kernel_drivers(&drivers, &PartitionId::new("work"), &["email".to_string()])
    .build()?;
let kernel = Kernel::builder().run_store(runs).drivers(drivers.clone()).build()?;
kernel.start().await?;
drivers.mount(DriverManifest::from_file("email.toml")?, "alice").await?;
# let _ = agent;
# Ok(())
# }
```

A partition serves a driver only when its manifest lists the partition and the partition's
own driver list names it. `toolset_for_manifests` serves every driver whose manifest lists
the partition, for a kernel that keeps no partition records, as `adk-rust up` does for the
default partition. `crossing_toolset` offers a coordinator other partitions' drivers as
crossings, each call under a non-waivable approval
([partitions](partitions.md#crossing-into-another-partitions-drivers)).

## Drivers in the managed form

A managed definition's driver manifests mount on the tenant's own supervisor, one person's
approval in the tenant's inbox per driver, and reach that tenant's partitions only; see
[the managed form](managed.md#a-definitions-drivers).

| Route | Purpose | `operator` | Recorded |
|---|---|---|---|
| `GET /api/kernel/drivers` | The mounted drivers with their state | — | — |
| `POST /api/kernel/drivers/{id}/mount` | `{"manifest": "<toml>", "confirm": false}` answers what the mount approves; `"confirm": true` mounts it as the caller | Yes | `drivers.mount` |
| `POST /api/kernel/drivers/{id}/unmount` | Unmount | Yes | `drivers.unmount` |
| `POST /api/kernel/drivers/{id}/restart` | Restart | Yes | `drivers.restart` |
| `POST /api/kernel/drivers/{id}/reapprove` | Re-approve as the caller | Yes | `drivers.reapprove` |

## Stores

Mounted drivers and parked calls are kept in a `DriverStore` beside the kernel's process
store: `SqliteDriverStore` in `harness.db`, `PostgresDriverStore` and `RedisDriverStore`
from `adk-rust up --store`, `InMemoryDriverStore` with `--ephemeral`, and, Alpha, in code,
`MongoDriverStore`, `FirestoreDriverStore` and `Neo4jDriverStore`
([backends](backends.md)). Each passes
`driver_store_conformance_tests!` (feature `conformance`).
