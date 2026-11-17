# Build an Autonomous Agent

An autonomous agent works toward a goal over many steps with no person at the keyboard, wakes
itself on a schedule or an event, stays inside the limits you set, survives a crash, and can be
watched and steered while it runs. This guide chooses among the ways ADK-Rust runs an agent
unattended, then builds one end to end on the `autonomous` template and the `autonomy`
feature.

## Choose a mechanism

Seven mechanisms run an agent without a person driving each turn. They share one governed tool
path, so policy, approvals, budgets, the action ledger and the kill switch apply in each.

| Mechanism | What it is | Wakes on | Survives a crash | Choose it when | Page |
|---|---|---|---|---|---|
| `AmbientAgent` | An event loop inside your program that runs an agent on each event | Cron, file changes, queues, webhooks your program subscribes to | Your program's concern: delivery is at most once | The agent is one part of a service you already run, and losing an event on a crash is acceptable | [Ambient agents](../agents/ambient-agents.md) |
| `DurableWorker` and `DurableScheduler` | Durable runs claimed by workers under leases, and a schedule store that enqueues them | Schedules, runs you enqueue | Yes: a stopped run is reclaimed and resumes; the action ledger keeps a call in flight from running twice | You host your own server and want runs, schedules and retries with no supervisor above them | [Durable runtime](../core/durable-runtime.md) |
| Server background runs | `/api/runs` and `POST /hooks/{name}` on `adk-server` (`background`, `webhook-ingress`) | HTTP requests, signed webhooks | Yes, through the durable runtime | Other systems start the agent over HTTP or webhooks | [Wake-ups and webhooks](../core/wake-ups.md) |
| The harness goal | `adk-rust goal`, or `goal` of a `HarnessLauncher` app: turns toward a goal until a check command passes, a separate grader passes a rubric, or both | You start it | Yes with a durable run store: `--resume` continues it | A coding or operations task with a verifiable finish line | [Plans and goals](../harness/plan-and-goals.md) |
| The agent kernel | A resident supervisor: a process table, wake sources, an approvals inbox, quotas, a control plane and the kill switch (`adk-rust up`, or `Kernel` in your program) | Cron, file changes, webhooks, approvals, budget refills | Yes: processes, schedules, held calls and the freeze are in its stores | The agent is always on: it sleeps between events, waits for decisions for days, and you operate it from other terminals | [Agent kernel](../kernel/index.md) |
| The managed runtime | `adk-managed`: declarative agents run as resumable sessions whose turns park on the client | Client events | Yes: a parked turn resumes on any instance over the same stores | You host agents for others as a platform, behind your own API | [Managed agents](../managed-agents/runtime.md) |
| The MCP agent server | `adk_server::mcp::McpAgentHost`: each MCP tool call a durable run, followed as an MCP task; a held call is asked as an elicitation and decided through the approval inbox | Tool calls from another agent or MCP client | Yes: the task is the run, so a restarted server answers for it | Another agent — yours, Claude, a LangChain agent — delegates long work to this one over MCP | [Serve an agent over MCP](../mcp/agent-server.md) |

The path below combines the last two that need no platform of your own: a harness agent whose
goals and schedule run on the agent kernel.

## What you build

`cargo adk new … --template autonomous` scaffolds an agent that keeps `STATUS.md` up to date
from the notes in `notes/` and publishes it. Replace the notes, the instruction and
`publish_status` with your own job; the wiring stays.

| Property | How the template does it |
|---|---|
| Works toward a goal | `cargo run -- goal "…" --until "…"`: turns until the check command exits 0 |
| Checks its own work | A grader in a separate context grades each iteration's `STATUS.md` against `rubric.md` (`[goal]` in `.adk/harness.toml`) and feeds the failed criteria back |
| Wakes itself | A cron `Trigger` on the kernel: each weekday at 09:00 UTC |
| Durable | One SQLite file, `.adk/state.db`: sessions, runs, held calls and decisions, the action ledger, spend, standing rules, modes, goals, the audit log, and the kernel's processes, cron cursors and kill switch |
| Governed | `auto` mode: reads, edits and sandboxed commands run; `publish_status`, an external tool, waits for approval |
| Sandboxed | `bash` in Seatbelt (macOS) or bubblewrap (Linux), set in `.adk/harness.toml` |
| Bounded | `RunBudget`: 40 model calls or 0.50 USD per run; `Quotas`: background work waits once the day's spend reaches 2 USD |
| Audited | Every tool decision on a signed hash chain, verifiable with `adk-rust audit verify` |
| Operable | `adk-rust ps`, `approvals`, `freeze`, `events` and `attach` with `--home .adk`, and steering over the kernel's socket |

## Before you start

| You need | Install |
|---|---|
| Rust 1.95 or later | [rustup](https://rustup.rs) |
| `cargo adk` | `cargo install cargo-adk` |
| `adk-rust`, the CLI | `cargo install adk-cli --features audit-sql` (`audit-sql` adds `adk-rust audit export`) |
| A model API key | Any provider `cargo adk` offers; this guide uses Anthropic |

## 1. Scaffold the agent

```bash
cargo adk new ops-agent --template autonomous --provider anthropic --model claude-sonnet-5-5
cd ops-agent
cp .env.example .env       # set ANTHROPIC_API_KEY
cargo test                 # publishing waits for a decision; no API key needed
```

The project depends on one crate:

```toml
adk-rust = { version = "3.0.0", default-features = false, features = ["anthropic", "autonomy", "cli", "minimal"] }
```

| File | Purpose |
|---|---|
| `src/main.rs` | The stores, the agent, the weekday trigger and the launcher |
| `src/publish.rs` | `publish_status`, the one side effect, and its test |
| `src/audit.rs` | The signed audit log and its key |
| `.adk/harness.toml` | `sandbox = true`; it can only tighten `src/main.rs` |
| `notes/` | What the agent reports on |

`src/main.rs` opens every store on one SQLite pool and hands each to the part that needs it:

```rust
let sessions = Arc::new(SqliteSessionService::new("sqlite://.adk/state.db?mode=rwc").await?);
sessions.migrate().await?;
let pool = sessions.pool().clone();
let stores = KernelStores::from_sqlite_pool(pool.clone()).await?;
// … standing rules, modes and goals on the same pool …
let approvals = Arc::new(
    StoreApprovalService::new(stores.approvals.clone()).with_run_store(stores.runs.clone()),
);
let harness = HarnessAgent::builder("ops-agent")
    .model(Arc::new(model))
    .workspace(Workspace::new(".").allow_bash(true))
    .mode(HarnessMode::Auto)
    .config(HarnessConfig::discover(".")?) // .adk/harness.toml: the OS sandbox
    .tool(publish::tool("outbox"))
    .approval_service(approvals)
    .action_ledger(stores.actions.clone())
    .spend_ledger(stores.spend.clone())
    .budget(RunBudget::new().max_model_calls(40).max_cost_usd(0.50))
    .audit_log(audit::open(&sessions, "ops-agent").await?)
    // … standing rules, mode store, goal store, prompter …
    .build()?;
HarnessLauncher::new(harness)
    .with_session_service(sessions)
    .with_kernel_stores(stores)
    .with_kernel_config(
        KernelConfig::default()
            .with_home(".adk")
            .with_quotas(Quotas::new().with_daily_spend_cap(2_000_000)),
    )
    .with_trigger(weekdays)
    .run()
    .await?;
```

`HarnessLauncher::run` gives the binary its commands: `chat` (the default REPL), `serve`, `up`
and `goal`.

## 2. Start the agent kernel

```bash
cargo run -- up
```

```text
ADK-Rust kernel up on http://127.0.0.1:8080
  socket .adk/kernel.sock
  home .adk
  mode auto · shell OS sandbox (seatbelt, no network)
  queues interactive, background, maintenance
  control: adk-rust ps | top | attach | kill | suspend | resume | freeze | events | proc
  approvals: GET /api/approvals, POST /api/approvals/{id}/decide
Press Ctrl+C to drain and stop
```

The banner follows the kernel's log lines. The kernel's home is `.adk`: its control socket,
`.adk/kernel.sock`, admits only your OS user — when the project's path is too long for a
socket, the banner names one in your temporary directory instead — and every `adk-rust`
control command reaches it with `--home .adk` from the project directory. `cargo run -- up`
refuses to start when any kernel store would be kept in memory.

## 3. Give it a goal and walk away

In a second terminal:

```bash
cargo run -- goal "Write STATUS.md from the notes in notes/ and publish it" \
  --until "test -s outbox/published.md"
```

```text
goal on the kernel at .adk · mode auto
goal:  Write STATUS.md from the notes in notes/ and publish it
until: test -s outbox/published.md
rubric: 4 criteria, graded at most 3 times; the grader reads STATUS.md
budget: 8 iterations
process 0803e865-8337-40cf-bbb9-32a8d4cac6dc · session 1e0281ee-df19-40ad-a380-1d78bf4a3f79
Ctrl-C detaches; the goal keeps running.

· glob {"pattern":"**/*"}
· read_file {"path":"notes/example.md"}
· read_file {"path":"README.md"}
· write_file {"content":"# Status\n\n- The release candidate passed the integration suite.\n- Two flaky tests in the payments module …
· publish_status {}
Tool confirmation required for 'publish_status'. Provide approve/deny decision to continue.

1 call(s) wait for a decision; the goal waits on the kernel and resumes once they are decided:
  apr_b6134e76e2bafcb16010237ac55cc6a9  publish_status {}
Decide with `adk-rust approvals --home .adk approve ID` (or `deny ID`), or from any surface.
```

The goal runs as a process of the kernel, as you, on its `background` queue. In `auto` mode the
agent reads the notes and writes `STATUS.md` without asking; `publish_status` is an external
tool, so the call waits in the kernel's approvals inbox. The terminal exits with `3` and the
goal waits on the kernel: close the terminal, or the laptop lid, and nothing is lost.

| Flag | Effect |
|---|---|
| `--mode ask\|auto` | The session's mode; the template's default is `auto` |
| `--sandbox` | Refuses to run unless `bash` is in the OS sandbox; the template's `.adk/harness.toml` turns it on |
| `--yes` | Approves every call that reaches the prompt, through the inbox while this terminal follows; never an unsandboxed retry without `--allow-unsandboxed` |
| `--max-iters N` | Turns toward the goal before it counts as exhausted (default 8) |
| `--rubric FILE`, `--rubric-from-goal` | The criteria a separate grader checks each iteration against; the template's `[goal]` table names `rubric.md` |
| `--deliverable PATH`, `--max-evaluations N`, `--rubric-hidden` | The files the grader reads, its grading rounds (default 3, at most 20), and criteria shown to the grader only |
| `--resume [--session ID]` | Continues the latest unfinished goal, or the one in `ID` |

The same flags work on `adk-rust goal`, the built-in coding agent: `adk-rust goal "…" --until
"…" --mode auto --sandbox` runs it unattended in any repository.

> **Note:** under Seatbelt on macOS, `auto` runs every sandboxed command without asking. Under
> bubblewrap on Linux, and in the Docker shell, `auto` runs sandboxed commands only while
> every root-level protected path exists ([details](../harness/shell-and-sandbox.md#auto_allow_sandboxed)),
> so commands wait for approval. On those hosts the 3.0.0 recipe for an unattended goal is
> `--yes`: `cargo run -- goal "…" --until "…" --yes`. `--yes` also approves `publish_status`,
> so leave it off when a person must decide the side effect.

## 4. Approve from another terminal

Hours later, from any terminal in the project directory:

```bash
adk-rust approvals --home .adk list
adk-rust approvals --home .adk approve apr_…
```

```text
$ adk-rust approvals --home .adk list
apr_b6134e76e2bafcb16010237ac55cc6a9  session 1e0281ee-df19-40ad-a380-1d78bf4a3f79  publish_status {}  (Tool 'publish_status' requires confirmation: 'publish_status' is an external too…)
$ adk-rust approvals --home .adk approve apr_b6134e76e2bafcb16010237ac55cc6a9
approved publish_status once; process 0803e865-8337-40cf-bbb9-32a8d4cac6dc continues now
```

The decision resumes the parked process at once: `publish_status` runs exactly once, the check
passes, and the grader checks `STATUS.md` against `rubric.md`. The goal ends once every
criterion passes; a failed criterion sends the agent back to revise with the grader's feedback,
for at most three grading rounds ([rubric-graded goals](../harness/plan-and-goals.md#rubric-graded-goals)). `adk-rust approvals --home .adk deny ID --reason "…"` refuses it
instead, and the agent is told why.

```text
$ adk-rust ps --home .adk
PID                                   STATE      KIND           QUEUE            AGE      TODAY      CALLS  NOTE
0803e865-8337-40cf-bbb9-32a8d4cac6dc  completed  goal           background        0s    $0.0443        7/6  goal 1/8, check exit 0
```

### Let a model decide first

An unattended agent still stops at every call `auto` holds. A `ModelApprover` reviews those
calls before they reach the inbox: it approves the routine ones, denies the ones the task does
not call for, and escalates the rest to you. It never decides a call the policy denies or a
non-waivable one — a protected path, an unsandboxed retry, a payment — and every verdict is an
audited inbox decision by `model:<model id>` that counts against the run's budget.

```rust
use std::sync::Arc;

use adk_core::ModelApprover;
use adk_harness::{HarnessAgent, HarnessMode};
use adk_model::{ScriptedLlm, ScriptedResponse};

fn main() {
    let model = Arc::new(ScriptedLlm::new("scripted", vec![ScriptedResponse::text("done")]));
    let builder = HarnessAgent::builder("status-agent")
        .model(model.clone())
        .mode(HarnessMode::Auto)
        .auto_review(Arc::new(ModelApprover::new(model).min_confidence(0.9)));
    let _ = builder;
}
```

`adk-rust goal … --mode auto --auto-review` and `adk-rust up --auto-review` do the same with
the session's model. Read [Model Reviewer](../security/tool-authorization.md#model-reviewer)
for its prompt-injection risk before you rely on it.

In Auto mode `web_fetch` and browser navigation also fetch only URLs that already appeared in
the conversation, in a user message or a tool result, so a page the agent reads cannot have
it carry data out in a URL it composed; a refused call is audited as `notInContext`. Set
`WebConfig::url_provenance` to `Always` to apply the rule in every mode, and
`WebConfig::fetch_domains` for hosts no approval may widen. Read
[URL provenance](../harness/web-and-mcp.md#url-provenance).

## 5. Let it wake itself

The template's trigger starts a run each weekday at 09:00 UTC:

```rust
let weekdays = Trigger::new(
    "weekday-status",
    Wake::Cron { expr: "0 0 9 * * Mon-Fri".into(), missed: MissedFirePolicy::FireOnce },
    NewRun::new(
        "background",
        "ops-agent",
        user.clone(),
        "ops-agent",
        adk_rust::serde_json::json!({ "text": "Refresh STATUS.md from notes/ and publish it if it changed." }),
    ),
);
```

A trigger lives in code, so a restart neither loses nor duplicates it. The expression has six
fields, seconds first, in UTC. `MissedFirePolicy::FireOnce` fires one run for the 09:00 a
stopped kernel missed; `Skip` fires only an occurrence that is on time, and `CatchUp(n)` up to
the last `n` ([missed occurrences](../kernel/scheduling.md#cron-wakes-and-missed-occurrences)).
A scheduled run that publishes holds `publish_status` in the inbox as the goal did.

To watch one fire now, set the expression to `"0 * * * * *"` (every minute), restart
`cargo run -- up`, and list the processes. Each run read the notes, found `STATUS.md`
current, and published nothing:

```text
$ adk-rust ps --home .adk          # three minutes after the restart
PID                                   STATE      KIND                      QUEUE            AGE      TODAY      CALLS  NOTE
b2fc1c55-edd0-4f63-8d00-a5df096b0aba  completed  triggered:weekday-status  background     2m01s    $0.0096        3/3
8c5e4a08-d476-4de2-9b77-b0683a0d1120  completed  triggered:weekday-status  background     1m01s    $0.0102        3/3
6b54fd29-baa1-4b6a-be3e-06bf954047a9  completed  triggered:weekday-status  background        1s    $0.0096        3/3
```

A webhook wakes the agent the same way: a `Trigger` on `Wake::Webhook { hook }` and a
`WebhookIngress` given to `HarnessLauncher::with_webhook_ingress`
([wake-ups](../core/wake-ups.md)).

## 6. Budgets and the kill switch

| Limit | Set in | What happens |
|---|---|---|
| Per run | `HarnessAgentBuilder::budget(RunBudget::new().max_model_calls(40).max_cost_usd(0.50))` | Once a limit is reached the run makes no further model call and ends `ResourceExhausted` with a `budget.*` code ([run budgets](../core/runner.md)) |
| Per day | `Quotas::new().with_daily_spend_cap(2_000_000)` (micro-USD) | A background or scheduled process sleeps with the label `kernel.admission: budget` until the next UTC day ([scheduling](../kernel/scheduling.md)) |
| Everything, now | `adk-rust freeze --home .adk --reason "…"` | Every process parks at its next checkpoint and nothing new starts, on every replica, until `adk-rust unfreeze --home .adk` |

```bash
adk-rust freeze --home .adk --reason "investigating the outbox"
adk-rust ps --home .adk
adk-rust unfreeze --home .adk
```

```text
$ adk-rust freeze --home .adk --reason "investigating the outbox"
frozen (investigating the outbox); 0 running process(es) parked on this replica, every replica parks its own. Lift it with `adk-rust unfreeze`.
$ cargo run -- goal "Add a Risks section to STATUS.md naming the quarantined tests, then publish it" --until "grep -q Risks outbox/published.md"
$ adk-rust ps --home .adk
PID                                   STATE      KIND           QUEUE            AGE      TODAY      CALLS  NOTE
0803e865-8337-40cf-bbb9-32a8d4cac6dc  completed  goal           background       33s    $0.0443        7/6  goal 1/8, check exit 0
9db0bd96-0408-429e-b54a-d9ea14244c9a  queued     goal           background       11s    $0.0000        0/0  goal 0/8
$ adk-rust unfreeze --home .adk
unfrozen; the processes the freeze parked resume.
```

A goal started while frozen waits `queued` and makes no model call; after `unfreeze` it runs.
The freeze is kept in `.adk/state.db`, so a kernel restarted while frozen stays frozen.

## 7. Crash and resume

Kill the kernel while a goal runs, then start it again:

```bash
kill -9 <pid of cargo run -- up>
cargo run -- up
```

The terminal following a goal stops with `server.unavailable` when the kernel dies; the goal
does not. After the restart the running goal is reclaimed once its 30-second lease lapses
(its age starts again at the new attempt) and finishes, and the call another goal held before
the crash is still in the inbox:

```text
$ adk-rust ps --home .adk          # before the kill: a goal mid-run, another holding a call
9db0bd96-0408-429e-b54a-d9ea14244c9a  waiting    goal           background       26s    $0.0116        4/6  goal 0/8
89cdfa94-9c82-4935-98b8-6556b2b0ccd6  running    goal           background        1s    $0.0000        1/1  goal 0/8
$ kill -9 84911
$ cargo run -- up
$ adk-rust ps --home .adk          # every 3 seconds after the restart
89cdfa94-9c82-4935-98b8-6556b2b0ccd6  running    goal           background       28s    $0.0000        1/1  goal 0/8
89cdfa94-9c82-4935-98b8-6556b2b0ccd6  running    goal           background        1s    $0.0000        1/1  goal 0/8
89cdfa94-9c82-4935-98b8-6556b2b0ccd6  running    goal           background        7s    $0.0106        4/7  goal 0/8
89cdfa94-9c82-4935-98b8-6556b2b0ccd6  completed  goal           background        2s    $0.0288        7/8  goal 1/8, check exit 0
$ adk-rust approvals --home .adk list
apr_1acf73b6f84320518841a0d9dbd2d2d4  session 110f88dc-588d-42fe-9716-9f431407d5b1  publish_status {}  (Tool 'publish_status' requires confirmation: 'publish_status' is an external too…)
```

| What | After the restart |
|---|---|
| A process that was running | Reclaimed once its lease lapses (30 seconds), as a new attempt, at its last checkpoint |
| A call in flight at the crash | Answered from the action ledger (`outcome_unknown` for a `NonIdempotent` call that began and never finished), never run twice |
| A held call | Still in the inbox; a decision resumes it |
| A sleeping process, the weekday trigger | Still asleep; a missed occurrence fires as its `MissedFirePolicy` says |
| A freeze | Still in force |

[Durability: after a crash](../harness/durability.md#after-a-crash) lists what to run for each
way the harness runs.

## 8. Observe and steer

| Question | Command |
|---|---|
| What runs, waits or sleeps | `adk-rust ps --home .adk` |
| What burns the budget | `adk-rust top --home .adk` |
| What happens, as it happens | `adk-rust events --home .adk --follow` |
| What the agent says in a session | `adk-rust attach --home .adk --session ID` |
| What it did, provably | `adk-rust audit export` and `adk-rust audit verify` |

Steer a running turn with a message it takes after its current tool step, before its next model
call ([steering](../kernel/control-plane.md#steering-a-running-turn)). Here a goal writes a
risk list —
`cargo run -- goal "Write RISKS.md: for each note, one line naming a risk it implies, then a line rating the overall risk" --until "test \$(wc -l < RISKS.md) -ge 4"`
— and the message narrows it while it runs. `SOCKET` is the `socket` line `up` printed;
`SESSION` is the session `goal` printed.

```bash
curl --unix-socket SOCKET -H 'content-type: application/json' \
  -d '{"text": "Keep RISKS.md under ten lines and rate the overall risk as low, medium or high."}' \
  http://localhost/api/kernel/sessions/SESSION/steer
```

```text
{"outcome":"accepted","steerId":"steer-7f0eb162-5dc8-422b-8f56-42e28c6a6e36","pid":"5d72ee56-4528-4361-91b6-f37dffdafe32","leaseEpoch":1}
```

`{"outcome":"returned"}` means no turn of the session takes messages yet — the process is
queued, between turns, or finished; send it again once `adk-rust ps` shows it `running`.
`adk-rust attach --home .adk --session SESSION` shows the message where the turn took it:

```text
You > Goal: Write RISKS.md: for each note, one line naming a risk it implies, then a line rating the overall risk…
· glob {"pattern":"notes/**"}
You > Keep RISKS.md under ten lines and rate the overall risk as low, medium or high.
· read_file {"path":"notes/example.md"}
· write_file {"content":"- Release candidate passed integration: the suite may not cover payments paths hidden by quarantined tests.\…
I wrote RISKS.md with 4 lines, which is under the ten-line limit. …
Goal check `test $(wc -l < RISKS.md) -ge 4` passed on iteration 1. The goal is done.
```

Verify the signed audit log:

```bash
adk-rust audit export --database-url sqlite://.adk/state.db --log-id ops-agent --out audit.jsonl
adk-rust audit verify audit.jsonl --key "$(cat .adk/audit.vkey)"
```

```text
log:                  ops-agent
origin:               adk-audit/ops-agent
records:              244 (0 pruned)
checkpoints verified: 4
signed through:       204
root:                 d96168f80119e79af0e235beaad0d9e5ac65d57b825677174e06010fc9594d79
unsigned tail:        40
result:               VALID
```

## 9. Delegate in the background

An agent that must not wait on long work starts
[background sub-agents](../agents/background-subagents.md). Each runs as its own kernel
process, on a budget slice carved from the parent's run and with no tool its parent lacks;
the parent keeps working and is told when it finishes. A sub-agent that holds a call shows
`waiting for a human` in `adk-rust ps` and waits in the same approvals inbox, and one running
when the kernel is killed resumes on restart. Background sub-agents are off until the kernel
is built with `KernelBuilder::subagents`, since each one works with no person watching it.
The limits that bound them — `max_depth`, `max_active` per session and the default budget —
are on `SubagentConfig`, and a tool policy can require approval for `start_subagent` like any
other call.

## The umbrella paths

Every page of these docs that imports a member crate has the same item under `adk-rust`:

| Member crate path | `adk-rust` path | Feature |
|---|---|---|
| `adk_core::X` | `adk_rust::X` | always |
| `adk_runner::X` | `adk_rust::runner::X` | `runner` |
| `adk_session::X` | `adk_rust::session::X` | `sessions` (`SqliteSessionService` and the SQLite stores with `autonomy`) |
| `adk_server::X` | `adk_rust::server::X` | `server` |
| `adk_auth::X` | `adk_rust::auth::X` | `auth` (`signed_audit` with `autonomy`) |
| `adk_harness::X`, `adk_devtools::Workspace` | `adk_rust::harness::X` | `harness` |
| `adk_kernel::X` | `adk_rust::kernel::X` | `harness-kernel` |
| `adk_cli::harness::HarnessLauncher` | `adk_rust::harness::HarnessLauncher` | `harness` and `cli` |

`autonomy` is in the `enterprise` and `full` tiers; on a smaller tier, add it with `cli`:
`features = ["minimal", "anthropic", "autonomy", "cli"]`.

## Read next

- [The harness](../harness/index.md): modes, approvals, the shell and goals
- [Agent kernel](../kernel/index.md): processes, wake sources and the control plane
- [Background sub-agents](../agents/background-subagents.md): delegation that does not wait
- [Tool authorization](../security/tool-authorization.md): policy, approvals and the kill switch
- [Remote sandbox providers](../sandbox/remote-providers.md): `bash` and agent code in E2B or
  Daytona microVMs, under the same kill switch and run budget
- [Governed computer use and browser use](../computer-use/toolsets.md): Claude's toolsets with
  each click, keystroke and navigation on the governed path
- [Spend ledger](../security/spend-ledger.md) and [audit log](../security/audit-log.md)
- [Model fallback](../models/fallback.md): keep an unattended run going through a provider
  outage, with every attempt counted against its budget
- `examples/autonomy_validation`: the governance and durability contracts above, checked live
  against OpenAI, Anthropic and Gemini
