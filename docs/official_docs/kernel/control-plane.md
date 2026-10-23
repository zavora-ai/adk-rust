# The Kernel Control Plane

Every surface reaches the [agent kernel](index.md) through one API on two transports: the
Unix socket at the kernel home, and HTTP (spec 3.2 §7.11). Surfaces hold no state:
disconnecting one changes nothing in the process table (K-I17).

## Transports and authentication

Every route needs an authenticated caller (K-I16); the routes that change the kernel also need
the `operator` scope.

| Transport | Caller | `operator` |
|---|---|---|
| Socket `<home>/kernel.sock` | The OS user, when the peer's uid is the socket owner's; created `0600`, any other peer is refused | Yes |
| HTTP on loopback (`adk-rust up` default) | Loopback peers as the OS user (`LoopbackOperatorExtractor`) | Yes |
| HTTP with `--auth-token` | `Authorization: Bearer <token>` (`BearerTokenExtractor`) | Yes |
| HTTP with an SSO extractor | The extractor's subject | When the extractor grants it |
| HTTP beyond loopback without a token or extractor | Refused at start (K-I18) | — |

| Answer | When |
|---|---|
| `401` | No authenticated caller |
| `403 kernel.unauthorized` | A change without the `operator` scope |
| `404 kernel.not_found` | An unknown process, session or target |
| `409 kernel.invalid_state` | The process is in a state the action does not apply to, such as resuming one a freeze parked |
| `422` | A body field the route does not take, such as `userId` on a spawn |

The socket needs a Unix platform. On Windows the control plane is served over HTTP alone;
the named pipe is a later deliverable.

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
| `/approvals…` | The approvals inbox, unchanged; the socket serves it too | Per inbox rules | Inbox records |

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
| `transport` | `socket` or `http` |
| `action` (ext) | `spawn`, `kill`, `nice`, `suspend`, `resume`, `freeze`, `unfreeze` or `attach` |
| `target` | The pid, session or spawned target |
| `pid` | The new process, on `spawn` |

`freeze` and `unfreeze` also write `governance.freeze` and `governance.unfreeze`. When the
freeze's own record fails, the freeze still takes effect, the kernel logs a warning, and the
answer carries the failure in `auditError`.

## From code

`adk_kernel::control::ControlClient` speaks the API over either transport:

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
`ServerBuilder::with_freeze_delegate`; `ControlPlane::start` serves it on the socket and, with
`ControlPlaneConfig::with_http`, on a bind of its own.
