# Kernel Commands

`adk-rust up` runs the [agent kernel](index.md) in the foreground; every other command
reaches a running kernel over its [control plane](control-plane.md) and holds no state of its
own (spec 3.2 §8.1, K-I17).

| Command | Does |
|---|---|
| `adk-rust up [--home DIR] [--bind HOST:PORT] [--auth-token T] [--store URL]` | Starts the kernel with the harness; the harness flags (`--mode`, `--dir`, `--sandbox`, …) apply |
| `adk-rust up --install [--print]` / `--uninstall` | Installs the kernel as a launchd agent, a systemd user unit or a Windows service that runs as you, logs to `<home>/kernel.log` and restarts on failure; `--print` shows the definition without installing; `--uninstall` removes it ([service installation](service.md)) |
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
| `adk-rust proc [ledger\|baselines\|observations]` | The activity ledger (`--export FILE` for JSONL), each partition's baselines, the observations ([`/proc`](proc.md)) |
| `adk-rust goal TEXT --until CMD` | With a kernel running, enqueues the goal on its `background` queue and follows it |
| `adk-rust drivers list\|mount FILE [--yes]\|unmount ID\|restart ID\|reapprove ID` | The kernel's [drivers](drivers.md): MCP servers it supervises under the manifests you mounted |
| `adk-rust pair [--name NAME] [--scope decide,read,stop] [--days 1-365]` | Issues a code that pairs one phone, tray or terminal ([pairing](pairing.md)) |
| `adk-rust pair list\|revoke ID\|accept CODE` | The paired devices and chat accounts, and those waiting; remove one, or dismiss a waiting one; pair the chat account that was sent `CODE` |
| `adk-rust partitions [list\|add ID\|set-mode P MODE]` | The kernel's [partitions](partitions.md) |

`PID` is a process id or a unique prefix of one.

## Reaching a kernel

Every control command takes the same three options:

| Option | Reaches |
|---|---|
| (none) | The socket at `<config dir>/adk-rust/kernel.sock`, or on Windows the named pipe of that home |
| `--home DIR` | The socket, or the named pipe, of the kernel started with `up --home DIR` |
| `--server URL [--token T]` | A kernel's HTTP control plane; the token defaults to `$ADK_SERVE_TOKEN` |

The socket and the pipe admit only the OS user that runs the kernel. Without a kernel the
command says so and exits non-zero:

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
  store harness.db: sessions, approvals, runs, processes, schedules, the activity ledger
  plans checkpointed in harness.db
ADK-Rust kernel up on http://127.0.0.1:8080
  socket /Users/ana/Library/Application Support/adk-rust/kernel.sock
  home /Users/ana/Library/Application Support/adk-rust
  mode ask · shell host (full user permissions, network)
  queues interactive, background, maintenance
  control: adk-rust ps | top | attach | kill | suspend | resume | freeze | events | proc
  approvals: GET /api/approvals, POST /api/approvals/{id}/decide
Press Ctrl+C to drain and stop
```

> **Note:** a Unix socket path is limited to about 100 bytes, so a deep `--home` is refused
> at start with that remedy; choose a short kernel home.

| Flag | Default | Notes |
|---|---|---|
| `--home DIR` | `[kernel] home`, else `<config dir>/adk-rust` | The socket, `harness.db`, the replica id and its audit signing key, and an installed service's `kernel.log` |
| `--bind HOST:PORT` | `[kernel] bind`, else `127.0.0.1:8080` | Anything but loopback needs `--auth-token` (K-I18) |
| `--auth-token T` | `$ADK_SERVE_TOKEN` | Bearer token HTTP callers present |
| `--store URL` | `harness.db` | `postgres://…` or `redis://…`: every store, the kernel's included, shared by replicas |
| `--no-memory`, `--no-consolidation` | memory and the tick on | Long-term memory stays on under `up`; the consolidation tick runs in ask and auto mode ([Long-term memory](../harness/memory.md#consolidation)) |
| `--install` | — | Installs the kernel as a service with the other flags, as you; refuses `--api-key`, `--auth-token`, a non-loopback `--bind` and a `--store` URL with a password ([service installation](service.md)) |
| `--print` | — | With `--install`: prints the service definition instead of installing it |
| `--uninstall` | — | Stops and removes the service of this kernel home; the home and its stores stay |

With `TELEGRAM_BOT_TOKEN` set, `up` also starts the Telegram chat thread, its accounts
paired with `adk-rust pair accept <code>` ([chat](../surfaces/chat.md#with-adk-rust-up)).

On Windows the kernel serves a named pipe in place of the socket, so the control commands
there reach it without `--server`.

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
| `[[kernel.partitions]]`, `coordinator` | The partitions `up` runs, and the one whose harness is the coordinator | Ignored, with a note |
| `drivers` | The driver manifests `up` mounts once you approve each ([below](#drivers)) | Ignored, with a note; its `.adk/drivers/<id>.toml` restricts the listed driver of that id |
| `[[kernel.webhooks]]` | The signed webhooks `up` serves at `/hooks/{name}` ([below](#webhooks)) | Ignored, with a note |

See [Proactivity](proactivity.md), [Scheduling](scheduling.md), [Partitions](partitions.md) and [Drivers](drivers.md) for what each does.

Flags win over both files. Unknown keys are reported and ignored.

### Partitions

With `[[kernel.partitions]]` entries, `up` runs one harness per [partition](partitions.md):
each in the partition's mode, its agents with the partition's class and tenant, its long-term
facts in the partition's memory scope (long-term memory is on for partitions under `up`, unless
`--no-memory`), and every request into another partition denied unless the coordinator makes
it, under approval.

```toml
[kernel]
coordinator = "home"

[[kernel.partitions]]
id = "home"
class = "home-agents"
mode = "ask"
cross_partition = "coordinator_with_approval"

[[kernel.partitions]]
id = "work"
class = "work-agents"
mode = "auto"
memory_scope = "work"
drivers = ["calendar"]
cross_partition = "coordinator_with_approval"
quiet_hours = "19:00-08:00"
quotas = { max_running = 2, max_queued = 10, daily_spend_cap_usd = 1.5 }
```

| Key of an entry | Default |
|---|---|
| `id` | — |
| `class` | the id |
| `tenant` | none |
| `mode` | `ask` |
| `memory_scope` | the id |
| `drivers` | none |
| `quiet_hours` | none; `HH:MM-HH:MM`, local time, held beside the kernel's |
| `quotas` | none; `max_running`, `max_queued`, `daily_spend_cap_usd`, each applied beside the kernel's quota, the lower one |
| `cross_partition` | `deny` |

At start, `up` adds each entry the partition table lacks; an entry the table already holds
keeps the table's record, which `adk-rust partitions` changes, and `up` notes the difference.
Each partition's harness runs as the targets `adk-rust@<partition>` and
`harness.goal@<partition>`; the coordinator's also runs `adk-rust` and `harness.goal`, so
`attach`, `goal` and `/api/runs` reach it. With the consolidation tick on, each partition in Ask
or Auto ticks on its own, distilling only the sessions its harness ran.

### Drivers

`drivers` lists the [driver manifests](drivers.md#the-manifest) `up` mounts when the kernel
starts. Each entry is a path or an inline manifest:

```toml
[kernel]
drivers = [
  "~/.adk/drivers/email.toml",   # a manifest file
  "drivers",                     # every *.toml manifest in a directory
  { id = "calendar", command = "mcp-calendar", partitions = ["work"], tools = { list_events = "read" } },
]
```

| Entry | Means |
|---|---|
| A path to a file | One manifest |
| A path to a directory | Every `*.toml` manifest in it, in file-name order |
| An inline table | A manifest written in place, with a manifest file's keys; `[[kernel.drivers]]` tables are the same |

`drivers = "~/.adk/drivers"` names one directory, the drivers directory of spec 3.2 §8.2. `~/`
is your home directory, and a relative path is relative to the directory of `harness.toml`. A
missing path, an invalid manifest or a driver listed twice stops `up` with the reason.

Mounting is approving (K-I13), so `up` asks before it mounts each listed driver that is not
mounted exactly as written:

```text
$ adk-rust up --mode ask
driver calendar
  command     mcp-calendar
  partitions  work
  sandbox     os
  network     allowed
  tools (any other tool is `other`):
    list_events              read
Mount this driver? [y/N] y
  store harness.db: sessions, approvals, runs, processes, agenda, schedules, drivers, partitions, the activity ledger, paired devices
  …
ADK-Rust kernel up on http://127.0.0.1:8080
```

| At start | `up` does |
|---|---|
| The driver is mounted as written | Nothing: the approval and the pinned declarations stay |
| It is not mounted, or its manifest changed since you approved it | Shows what you approve and mounts it once you answer `y` |
| No terminal, as under a service | Mounts nothing new and notes each driver waiting for approval; a changed driver keeps running as you approved it |

Mounting records `kernel.driver.mounted` with you as the actor, as `adk-rust drivers mount`
does. A workspace's `.adk/drivers/<id>.toml` restricts the listed driver of the same id. Taking
a driver out of the list does not unmount it: `adk-rust drivers unmount ID` does.

### Webhooks

`adk-rust up` serves the hooks the user config lists at `POST /hooks/{name}` (spec 3.5 §10
X11). Each delivery is checked against its Standard Webhooks secret and the kill switch, and
becomes a `kernel.wake` run keyed `hook:<name>:<webhook-id>`, so it wakes the processes asleep on
that hook — `adk-rust wake --webhook <name>` and the partitions' triggers — and a redelivery
wakes nothing twice.

```toml
[[kernel.webhooks]]
name = "github"
secret = "env:GITHUB_WEBHOOK_SECRET"       # or "file:github.whsec", relative to this file
```

| Answer | When |
|---|---|
| `202 Accepted` with `{"runId", "state", "duplicate": false}` | A new delivery |
| `200 OK` with `"duplicate": true` | A redelivery of a `webhook-id` that already has a run |
| `401` | A missing, expired or wrong signature |
| `503` | The kernel is frozen |

The secret is a `whsec_` base64 secret read from the environment or a file, never written in
`harness.toml` itself. Banner line: `hooks /hooks/github (signed webhooks wake the kernel's
sleepers)`. The hooks keep the server's host and origin checks, so a sender reaching the kernel
through a proxy needs that proxy's host allowed.

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
`--ephemeral`, `--continue` and `--voice` keep the harness in this process, as the
[harness](../harness/cli.md) REPL or, for `--voice` on a terminal, the terminal's voice mode.

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
| `approve ID` | Approves a proposed item as you; it runs once admitted. For an `observation`, confirms it: nothing runs, and its statement is remembered as known ([`/proc`](proc.md#answering-an-observation)) |
| `reject ID [--reason R]` | Rejects a proposed or approved item; for an `observation`, dismisses it with nothing remembered |
| `schedule ID --at TIME` | Runs the item at a time — RFC 3339, `HH:MM` local, or `+30m`, `+2h`, `+1d` — approving a proposed item first; refused for an `observation`, which is answered, never scheduled |
| `schedule ID --cron EXPR` | At the next time the cron expression (UTC) fires |

An ID may be a unique prefix. An item the consolidation tick or the baseline pass proposed is
approved only by a person; `agenda approve` over the socket or loopback is one.

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
| `--webhook NAME` | A signed delivery reaches `POST /hooks/NAME` on `adk-rust up`, which serves the hooks `[[kernel.webhooks]]` lists ([below](#webhooks)) |

The process runs on the `background` queue, in a new session unless `--session ID`;
`adk-rust ps` shows it `waiting`, asleep on its wake set. A file or webhook sleeper wakes once.
A cron sleeper recurs: the kernel fires the expression itself on its clock (one wake per
occurrence across replicas) and, when a run completes or fails, starts its successor asleep
with the finished run as its parent; `adk-rust kill` on the sleeper ends the series. The
expression's cursor is kept in `harness.db` (or the `--store` backend), so after the kernel was
down it fires the missed occurrence once if it is no more than a minute late, the sleeper's
`skip` policy, and never one that already fired. A new series replays nothing from before it
began.

## `adk-rust partitions`

```text
$ adk-rust partitions
ID           CLASS          MODE       MEMORY         DRIVERS              CROSSINGS                  BLOCKED
home         home-agents    ask        home           -                    coordinator_with_approval  -
work         work-agents    auto       work           calendar             coordinator_with_approval  -
$ adk-rust partitions set-mode work ask
$ adk-rust partitions add team --driver tracker --coordinator-crossings
```

| Subcommand | Does |
|---|---|
| `list` (the default) | The partition table |
| `add ID [--class C] [--tenant T] [--mode M] [--memory-scope S] [--driver D]… [--coordinator-crossings]` | Adds a partition, or replaces one; its harness starts at the next `up` |
| `set-mode P MODE` | Sets a partition's mode: `read_only`, `plan`, `ask` or `auto`; its harness takes it at the next `up`, over a mode changed in the home before |

`add` and `set-mode` need the `operator` scope and are recorded as `kernel.control` and
`kernel.partition.created` or `kernel.partition.changed`.

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
