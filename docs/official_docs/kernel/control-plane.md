# The Kernel Control Plane

Every surface reaches the [agent kernel](index.md) through one API on three transports: the
Unix socket at the kernel home, a named pipe named after it on Windows, and HTTP (spec 3.2
§7.11). Surfaces hold no state:
disconnecting one changes nothing in the process table (K-I17).

## Transports and authentication

Every route needs an authenticated caller (K-I16); the routes that change the kernel also need
the `operator` scope.

| Transport | Caller | `operator` |
|---|---|---|
| Socket `<home>/kernel.sock` | The OS user, when the peer's uid is the socket owner's; created `0600`, any other peer is refused | Yes |
| Named pipe `\\.\pipe\adk-rust-kernel-<tag>` (Windows) | The OS user, when the token the client opened the pipe with is the kernel's user; any other client is refused | Yes |
| HTTP on loopback (`adk-rust up` default) | Loopback peers as the OS user (`LoopbackOperatorExtractor`) | Yes |
| HTTP with `--auth-token` | `Authorization: Bearer <token>` (`BearerTokenExtractor`) | Yes |
| HTTP with an SSO extractor | The extractor's subject | When the extractor grants it |
| HTTP beyond loopback without a token or extractor | Refused at start (K-I18) | — |
| HTTP with a paired device's token (`Authorization: Bearer adkd_…`) | The person who paired the device, only on the routes its scopes open, on loopback too ([pairing](pairing.md)); the socket and the pipe read no device token | Only for `POST /kernel/freeze`, with the `stop` scope |

| Answer | When |
|---|---|
| `401` | No authenticated caller |
| `403 kernel.unauthorized` | A change without the `operator` scope |
| `404 kernel.not_found` | An unknown process, session or target |
| `404 kernel.agenda_not_found` | An unknown agenda item |
| `409 kernel.invalid_state` | The process is in a state the action does not apply to, such as resuming one a freeze parked |
| `409 kernel.agenda_state` | The agenda item is in a state the action does not apply to |
| `409 kernel.spawn_rejected` | Another run holds the spawn's concurrency key and the run does not queue behind it |
| `429 kernel.quota` | A partition quota refuses the process |
| `422` | A body field the route does not take, such as `userId` on a spawn |

### The named pipe on Windows

Windows has the named pipe where Unix has the socket (spec 3.2 §10 W2, W4). Pipe names are
not files, so the name carries a tag derived from the kernel home's socket path
(`ControlPlaneConfig::pipe_name`), and each guard below stands in for what the home
directory and mode `0600` give the socket:

| Guard | What it stops |
|---|---|
| A protected DACL with one entry, full access for the user the kernel runs as, who also owns the pipe | Any other account opening the pipe or creating an instance of it |
| The server reads the client's first message, impersonates it at identification level and compares its token's user with its own | A client of another identity the DACL let through |
| The client uses the pipe only when the kernel's user owns it | Another account's process that took the name first answering in the kernel's place |
| The first instance is created with `FILE_FLAG_FIRST_PIPE_INSTANCE`; remote clients are rejected | Joining a pipe someone else created; callers from another machine |

A second kernel on the same home is refused while the first answers, as on the socket.

## Routes

All routes are under `/api`, on both transports. `adk-rust up` merges them into its
`ServerBuilder` with `add_api_routes`, so the server's auth middleware applies.

| Route | Purpose | `operator` | Recorded |
|---|---|---|---|
| `GET /kernel/health` | State, frozen, per-worker health, scheduler lag, store latency | — | — |
| `GET /kernel/processes?partition=&state=&limit=` | The process table, each process with its live view | — | — |
| `POST /kernel/processes` | Start a process of a registered target as the caller | Yes | `spawn` |
| `POST /kernel/processes/{pid}/kill` | Cancel at the next checkpoint; body `{"reason": "..."}` | Yes | `kill` |
| `POST /kernel/processes/{pid}/nice` | Move a queued or sleeping process to another queue; body `{"queue": "background"}`; the moved process has a new pid | Yes | `nice` |
| `POST /kernel/processes/{pid}/suspend` | Park at the next model-call boundary | Yes | `suspend` |
| `POST /kernel/processes/{pid}/resume` | Wake a suspended or sleeping process | Yes | `resume` |
| `GET /kernel/events?after=` | The event bus as server-sent events from a cursor; `Last-Event-ID` wins over `after` | — | — |
| `POST /kernel/attach/{session}` | Transcript since a cursor, held calls, the process running the session | — | `attach` |
| `POST /kernel/freeze`, `POST /kernel/unfreeze` | `Kernel::freeze` and `Kernel::unfreeze`; the answer reports `parkedRuns` | Yes | `freeze`, `unfreeze` |
| `GET /kernel/drivers` | The mounted [drivers](drivers.md) with their state | — | — |
| `POST /kernel/drivers/{id}/mount` | `{"manifest": "<toml>"}` answers what the mount approves; with `"confirm": true` mounts it as the caller | Yes | `drivers.mount` |
| `POST /kernel/drivers/{id}/{unmount,restart,reapprove}` | Unmount, restart, re-approve as the caller | Yes | `drivers.unmount`, `drivers.restart`, `drivers.reapprove` |
| `POST /kernel/pairing` | Issue a device pairing code ([pairing](pairing.md)) | Yes | `pairing.issue` |
| `POST /kernel/pairing/claim` | Claim a code; needs no caller, the code is the credential | — | `pairing.claim` |
| `GET /kernel/devices`, `POST /kernel/devices/{id}/revoke` | The paired devices; remove one | Yes | `devices.revoke` |
| `POST /kernel/channels/pair`, `GET /kernel/channels`, `POST /kernel/channels/{id}/revoke`, `POST /kernel/channels/waiting/{id}/dismiss` (feature `channels`) | Accept the code a chat account was sent; the paired and waiting accounts; remove one; dismiss a waiting one | Yes | `channels.pair`, `channels.revoke`, `channels.dismiss` |
| `GET /kernel/proc/ledger?partition=&kind=&agent=&from=&to=&after=&limit=&format=` | A page of the activity ledger, or JSONL with `format=jsonl` ([`/proc`](proc.md)) | Yes | `proc.ledger` |
| `GET /kernel/proc/baselines`, `GET /kernel/proc/observations?partition=&since=&limit=` | Each partition's last baseline pass; the observations | Yes | — |
| `GET /kernel/partitions` | The [partition](partitions.md) table | — | — |
| `POST /kernel/partitions` | Create or replace a partition; body: the partition record | Yes | `partition.put` |
| `POST /kernel/partitions/{id}/mode` | Set a partition's mode; body `{"mode": "auto"}` | Yes | `partition.mode` |
| `/approvals…` | The approvals inbox, unchanged; the socket and the pipe serve it too | Per inbox rules | Inbox records |

On HTTP the server's own routes stay beside them: `/api/runs` (submissions land on the
`interactive` queue), `/api/admin/freeze` (delegates to `Kernel::freeze` and reports
`parkedRuns`), `/api/admin/governance` (`runningRuns: "park"`), `/api/rules` and the session
routes.

### Starting a process

```bash
curl --unix-socket "$HOME/Library/Application Support/adk-rust/kernel.sock" \
  -H 'content-type: application/json' \
  -d '{"target": "adk-rust", "input": {"text": "summarize CHANGELOG.md"}, "sessionId": "s-1"}' \
  http://localhost/api/kernel/processes
```

| Field | Default | Notes |
|---|---|---|
| `target` | — | A target the kernel registered: `adk-rust` or `harness.goal`; another answers `404` naming them |
| `input` | `null` | A string, `{"text": ...}` or `{"content": ...}` |
| `sessionId` | A new session | The session an agent run continues |
| `queue` | `interactive` | `interactive`, `background`, `maintenance` or a named queue |
| `kind` | `interactive` | `goal`, `scheduled`, `{"triggered": {"source": ...}}`, … |
| `wake` | — | A wake set: the process starts asleep |
| `parent`, `labels` | — | Lineage and free-form labels |

The run belongs to the authenticated caller and is marked authenticated, as `/api/runs`
marks one.

### Attaching to a session

```bash
curl --unix-socket "$HOME/Library/Application Support/adk-rust/kernel.sock" \
  -H 'content-type: application/json' -d '{"after": "2026-10-22T10:42:07Z"}' \
  http://localhost/api/kernel/attach/s-1
```

The answer carries `transcript` (the session's events after `after`), `cursor` (the last
event's time, for the next attach), `pending` (the calls the session holds for a decision) and
`process` (the unfinished process running the session, if any). A surface attaches only to
its caller's own sessions.

## Audit records

Every recorded action writes `kernel.control` to the kernel's signed audit log before it
acts, and is refused when the record cannot be written, except `freeze`, which takes effect
regardless:

| Field | Value |
|---|---|
| `kind` | `Other("kernel")` |
| `action` | `kernel.control` |
| `actor` | The caller as a `human` subject |
| `subject` | `{"kind": "human", "id": "<user>"}` |
| `transport` | `socket`, `pipe` or `http` |
| `action` (ext) | `spawn`, `kill`, `nice`, `suspend`, `resume`, `freeze`, `unfreeze`, `attach`, or a pairing action: `pairing.issue`, `pairing.claim`, `devices.revoke`, `channels.pair`, `channels.revoke`, `channels.dismiss`, `approvals.decide` |
| `target` | The pid, session or spawned target |
| `pid` | The new process, on `spawn` |
| `device` | The paired device, on an action a device took |

`freeze` and `unfreeze` also write `governance.freeze` and `governance.unfreeze`. When the
freeze's own record fails, the freeze still takes effect, the kernel logs a warning, and the
answer carries the failure in `auditError`.

## From code

`adk_kernel::control::ControlClient` speaks the API over every transport;
`ControlClient::connect_local` finds the socket, or the pipe on Windows. A refusal reaches
the caller with the kernel's code from the table above; a code the client does not know reads
as `kernel.control_plane`.

```rust
use adk_kernel::control::ControlClient;
use adk_kernel::control::api::{ProcessesQuery, SpawnRequest};

async fn list(home: &std::path::Path) -> adk_kernel::Result<()> {
    let client = match ControlClient::connect_local(home).await? {
        Some(client) => client,
        None => ControlClient::http("https://kernel.example.com").with_bearer_token("token"),
    };
    let turn = SpawnRequest::new("adk-rust", serde_json::json!({ "text": "status?" }));
    let process = client.spawn(&turn).await?;
    println!("started {}", process.meta.pid);
    for process in client.processes(&ProcessesQuery::default()).await? {
        println!("{} {:?} {}", process.meta.pid, process.run.state, process.meta.kind);
    }
    for held in client.approvals().await? {
        println!("{} waits: {}", held.id, held.tool_name);
    }
    Ok(())
}
```

To serve the control plane from your own server, merge `adk_kernel::control::router(kernel)`
with `ServerBuilder::add_api_routes` and register the kernel with
`ServerBuilder::with_freeze_delegate`; `ControlPlane::start` serves it on the socket (the pipe
on Windows) and, with `ControlPlaneConfig::with_http`, on a bind of its own.

`KernelConfig::default()` serves no control plane, so a kernel embedded in an application
never takes the socket of the kernel `adk-rust up` runs at the same home. Pass
`KernelConfig::with_control(ControlPlaneConfig::default())` to serve the socket at the
kernel's home, as `adk-rust up` does:

```rust
use adk_kernel::KernelConfig;
use adk_kernel::control::ControlPlaneConfig;

let config = KernelConfig::default()
    .with_home("/srv/kernel")
    .with_control(ControlPlaneConfig::default());
assert!(config.control.socket.is_some());
```
