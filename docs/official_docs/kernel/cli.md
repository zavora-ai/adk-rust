# Kernel Commands

`adk-rust up` runs the [agent kernel](index.md) in the foreground; every other command
reaches a running kernel over its [control plane](control-plane.md) and holds no state of its
own (spec 3.2 §8.1, K-I17).

| Command | Does |
|---|---|
| `adk-rust up [--home DIR] [--bind HOST:PORT] [--auth-token T] [--store URL]` | Starts the kernel with the harness; the harness flags (`--mode`, `--dir`, `--sandbox`, …) apply |
| `adk-rust attach [--session ID]` | A REPL whose turns the kernel runs |
| `adk-rust` | Attaches when a kernel runs at the default home; otherwise the in-process REPL |
| `adk-rust ps [--partition P] [--state S] [--limit N]` | The process table |
| `adk-rust top [--partition P] [--interval SECS]` | Unfinished processes by burn rate, refreshing until Ctrl-C |
| `adk-rust kill PID [--reason R]` | Cancels a process at its next checkpoint |
| `adk-rust suspend PID` / `resume PID` | Parks a process at its next model-call boundary / wakes it |
| `adk-rust nice PID QUEUE` | Moves a queued or sleeping process to `interactive`, `background` or `maintenance`; the moved process gets a new pid |
| `adk-rust agenda [list\|approve ID\|reject ID\|schedule ID --at TIME\|--cron EXPR]` | The agenda: follow-ups proposed for later ([Proactivity](proactivity.md)) |
| `adk-rust wake --file GLOB\|--cron EXPR\|--webhook NAME -- "task"` | A sleeping process that runs the task when it wakes |
| `adk-rust freeze [--reason R]` / `unfreeze` | The kill switch, on every replica |
| `adk-rust events [--follow] [--after CURSOR] [--json]` | The event bus |
| `adk-rust goal TEXT --until CMD` | With a kernel running, enqueues the goal on its `background` queue and follows it |

`PID` is a process id or a unique prefix of one.

## Reaching a kernel

Every control command takes the same three options:

| Option | Reaches |
|---|---|
| (none) | The socket at `<config dir>/adk-rust/kernel.sock` |
| `--home DIR` | The socket of the kernel started with `up --home DIR` |
| `--server URL [--token T]` | A kernel's HTTP control plane; the token defaults to `$ADK_SERVE_TOKEN` |

The socket admits only the OS user that runs the kernel. Without a kernel the command says
so and exits non-zero:

```text
$ adk-rust ps
Error: no kernel runs at /Users/ana/Library/Application Support/adk-rust: start one with `adk-rust up`, or reach one over HTTP with --server URL
```

## `adk-rust up`

```bash
adk-rust up                                        # plan mode, 127.0.0.1:8080, harness.db
adk-rust up --mode ask --dir ./project             # edits and commands after approval
adk-rust up --bind 0.0.0.0:8080 --auth-token "$TOKEN"
adk-rust up --store postgres://harness:…@db/harness --bind 0.0.0.0:8081 --auth-token "$TOKEN"
adk-rust up --home /srv/kernel                     # socket and harness.db under /srv/kernel
```

```text
$ adk-rust up --mode ask
  store harness.db: sessions, approvals, runs, processes, schedules
  plans checkpointed in harness.db
ADK-Rust kernel up on http://127.0.0.1:8080
  socket /Users/ana/Library/Application Support/adk-rust/kernel.sock
  home /Users/ana/Library/Application Support/adk-rust
  mode ask · shell host (full user permissions, network)
  queues interactive, background, maintenance
  control: adk-rust ps | top | attach | kill | suspend | resume | freeze | events
  approvals: GET /api/approvals, POST /api/approvals/{id}/decide
Press Ctrl+C to drain and stop
```

> **Note:** a Unix socket path is limited to about 100 bytes, so a deep `--home` is refused
> at start with that remedy; choose a short kernel home.

| Flag | Default | Notes |
|---|---|---|
| `--home DIR` | `[kernel] home`, else `<config dir>/adk-rust` | The socket, `harness.db`, the replica id and its audit signing key |
| `--bind HOST:PORT` | `[kernel] bind`, else `127.0.0.1:8080` | Anything but loopback needs `--auth-token` (K-I18) |
| `--auth-token T` | `$ADK_SERVE_TOKEN` | Bearer token HTTP callers present |
| `--store URL` | `harness.db` | `postgres://…` or `redis://…`: every store, the kernel's included, shared by replicas |
| `--no-memory`, `--no-consolidation` | memory and the tick on | Long-term memory stays on under `up`; the consolidation tick runs in ask and auto mode ([Long-term memory](../harness/memory.md#consolidation)) |
| `--install`, `--uninstall` | — | Service installation is not in this build: run `up` under launchd, systemd or a container supervisor |

On Windows the kernel serves HTTP only: the named-pipe transport is a later deliverable, so
control commands there take `--server`.

### `[kernel]` in `harness.toml`

The user config at `<config dir>/adk-rust/harness.toml` sets the kernel; a workspace's
`.adk/harness.toml` may only restrict it:

```toml
[kernel]
home = "/srv/kernel"
bind = "127.0.0.1:9090"
max_concurrent_runs = 4
quiet_hours = "22:00-07:00"

[kernel.proactivity]
max_proposed_open = 10
daily_budget_usd = 2.0
approval = "follow_mode"   # always_ask, never
review_days = 90

[kernel.quotas]
max_running = 3
max_queued = 20
daily_spend_cap_usd = 5.0

[kernel.consolidation]
enabled = true
min_new_sessions = 3
min_new_facts = 25
min_interval_minutes = 360
debounce_minutes = 30
max_tokens_per_tick = 50000
```

| Key | User config | Workspace file |
|---|---|---|
| `home` | Applies | Ignored, with a note |
| `bind` | Applies | Ignored, with a note |
| `max_concurrent_runs` | Applies to each queue's worker | Applies only when lower |
| `quiet_hours` | `HH:MM-HH:MM`, local time | A second window, applied beside the user's |
| `proactivity.max_proposed_open`, `daily_budget_usd`, `review_days` | Apply | Apply only when lower |
| `proactivity.approval` | Applies | Applies only when stricter: `never` over `always_ask` over `follow_mode`; `never` turns proactivity off |
| `quotas.max_running`, `max_queued`, `daily_spend_cap_usd` | Apply to every partition | Apply only when lower |
| `consolidation.enabled` | `false` turns the tick off | `false` turns it off |
| `consolidation.min_new_sessions`, `min_new_facts`, `min_interval_minutes`, `debounce_minutes` | The tick's admission gates | Ignored, with a note |
| `consolidation.max_tokens_per_tick` | The tick's token cap | Applies only when lower |

See [Proactivity](proactivity.md) and [Scheduling](scheduling.md) for what each does.

Flags win over both files. Unknown keys are reported and ignored.

## `adk-rust attach` and the REPL on a kernel

```text
$ adk-rust attach
Attached to the kernel at unix:/Users/ana/Library/Application Support/adk-rust/kernel.sock (running).
Session 1f0c…: each turn runs as a process the kernel owns. `exit` or Ctrl-D detaches; the kernel keeps running.

You > run the tests and fix what fails
· bash {"command":"cargo test"}

⏸ bash {"command":"cargo test"}
Approve? [y]es, [n]o, or a reason to refuse > y
approved
```

| Behaviour | Detail |
|---|---|
| A turn | `POST /api/kernel/processes` with the line, on the `interactive` queue, as the caller |
| The transcript | Read back by cursor from `POST /api/kernel/attach/{session}`; `--session ID` replays the session first |
| A held call | Asked here and decided through `/api/approvals`; any other surface may decide it instead |
| Ctrl-C while a turn runs | Detaches from the turn; the kernel carries on |
| `exit`, Ctrl-D | Detaches; the session stays on the kernel |

`adk-rust` with no subcommand attaches when a kernel answers on the default socket.
`--ephemeral`, `--continue` and `--voice` keep the in-process REPL of the [harness](../harness/cli.md).

## `adk-rust ps` and `top`

```text
$ adk-rust ps
PID                                   STATE      KIND           QUEUE            AGE      TODAY      CALLS  NOTE
78136dc9-e826-4b4f-b6f5-8d369c8cfb75  waiting    interactive    interactive       1s    $0.0000        0/0  asleep on 1 wake(s)
```

| Column | Source |
|---|---|
| `AGE` | Since the last state change: a sleeper's time asleep, a parked run's wait |
| `TODAY` | Spend today (UTC) from the spend ledger |
| `CALLS` | Model calls / tool calls |
| `NOTE` | Suspension (`suspended: freeze`), sleep (`asleep on N wake(s)`), goal progress and last check (`goal 3/10, check exit 1`), loop-guard verdict, a failure's error |

`top` adds `BURN/MIN` and `BY 24:00` (the spend projected to the end of the UTC day at the
current rate) and lists only unfinished processes, highest burn first.

## `adk-rust goal` on a running kernel

With a kernel running at the default home over `harness.db`, `goal` writes the goal to the
session and enqueues it on the kernel's `background` queue instead of running a worker of its
own, then follows it:

| Outcome | Exit code |
|---|---|
| The goal's `--until` passed | `0` |
| It ran out of iterations or stopped | `1` |
| A call waits for a decision and no terminal answered it | `3`; the goal waits on the kernel |
| Ctrl-C | `0`; the goal keeps running on the kernel |

`--resume` and `--ephemeral` run the goal in this process, as without a kernel.

## `adk-rust agenda`

```text
$ adk-rust agenda
ID                                    STATE       ORIGIN             AGE  TITLE
ab264c4b-f8f5-413e-937a-b364be35b52f  proposed    agent               3s  Write a status note about the dependency bump

1 proposed: `adk-rust agenda approve ID`, `reject ID` or `schedule ID --at TIME`.
$ adk-rust agenda approve ab264c4b
approved ab264c4b-f8f5-413e-937a-b364be35b52f: now scheduled on 1 wake(s)
```

| Subcommand | Does |
|---|---|
| `list [--partition P] [--state S] [--limit N]` | The items, oldest first; the default |
| `approve ID` | Approves a proposed item as you; it runs once admitted |
| `reject ID [--reason R]` | Rejects a proposed or approved item |
| `schedule ID --at TIME` | Runs the item at a time — RFC 3339, `HH:MM` local, or `+30m`, `+2h`, `+1d` — approving a proposed item first |
| `schedule ID --cron EXPR` | At the next time the cron expression (UTC) fires |

An ID may be a unique prefix. An item the consolidation tick proposed is approved only by a
person; `agenda approve` over the socket or loopback is one.

## `adk-rust wake`

```bash
adk-rust wake --file Cargo.lock -- "audit the new dependencies"
adk-rust wake --cron "0 0 8 * * Mon-Fri" -- "summarize last night's CI failures"
adk-rust wake --webhook github -- "triage the new issue"
```

```text
$ adk-rust wake --file Cargo.lock -- "audit the new dependencies"
dc30e0e1-3b02-4f6a-b681-5884e9883f54 sleeps until Cargo.lock changes under /home/ana/project; `adk-rust ps` shows it.
$ adk-rust wake --cron "0 0 8 * * Mon-Fri" -- "summarize CI"
64551be2-d2e1-407e-92e2-8c96efea1597 sleeps until `0 0 8 * * Mon-Fri` (UTC) fires, first at 2026-10-23 08:00:00, and again after each run until killed; `adk-rust ps` shows it.
```

| Flag | Sleeps until |
|---|---|
| `--file GLOB` | A file matching the pattern changes, relative to the current directory; the kernel watches the directory |
| `--cron EXPR` | The expression (UTC) fires; after each run the kernel puts a successor to sleep on the same wake, so it runs at every occurrence until you kill the sleeper |
| `--webhook NAME` | The hook delivers to an ingress on the kernel's store (`webhook_wake_hook`); `adk-rust up` serves no hooks itself |

The process runs on the `background` queue, in a new session unless `--session ID`;
`adk-rust ps` shows it `waiting`, asleep on its wake set. A file or webhook sleeper wakes once.
A cron sleeper recurs: the kernel fires the expression itself on its clock (one wake per
occurrence across replicas) and, when a run completes or fails, starts its successor asleep
with the finished run as its parent; `adk-rust kill` on the sleeper ends the series.

## `adk-rust events`

```text
$ adk-rust events
#1      00:32:39  health     kernel running
#2      00:32:40  process    78136dc9-e826-4b4f-b6f5-8d369c8cfb75 queued → queued (spawned asleep)
#3      00:32:40  process    78136dc9-e826-4b4f-b6f5-8d369c8cfb75 waiting → running (claimed)
#4      00:32:40  process    78136dc9-e826-4b4f-b6f5-8d369c8cfb75 running → waiting (asleep)
#5      00:32:42  process    78136dc9-e826-4b4f-b6f5-8d369c8cfb75 waiting → cancelled (killed by ana over the kernel control plane)
```

Without `--follow` the command prints what the kernel still holds after `--after` (default:
from the start) and exits once the stream is quiet for a second. `--json` prints one
`EventRecord` per line.
