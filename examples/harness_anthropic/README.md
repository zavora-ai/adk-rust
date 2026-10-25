# harness_anthropic

The `adk-harness` walkthrough on Anthropic: nine scenarios that show what `HarnessAgent` adds to
an agent loop, from approvals bound to the tool to goals that survive the process (spec 3.1
§5). Every check is on files, stores and audit records, never on what the model says.

The scenarios live in [`examples/harness_walkthrough`](../harness_walkthrough) and are shared with
[`examples/harness_openai`](../harness_openai); this crate supplies the Anthropic model. Each scenario
runs the same code scripted and live: a scripted run replays, through `adk_model::ScriptedLlm`,
what a model that follows the prompt does; a live run sends the same prompts to `claude-sonnet-5-5`.

## Run

```bash
cargo run                                # every scenario, scripted: no key
cp .env.example .env                     # then set ANTHROPIC_API_KEY
cargo run -- --live                      # every scenario on claude-sonnet-5-5
cargo run -- --live approvals sandbox    # selected scenarios
cargo run -- --live --model <model-id>   # another Anthropic model (ANTHROPIC_MODEL works too)
```

`HARNESS_ENV_FILE=/path/to/.env` loads the key from another file. The scenarios build and test a
small crate, so `cargo` must be on `PATH`. The run exits with status 1 when a scenario fails.

## Scenarios

| Scenario | Spec 3.1 | What runs | What is checked |
|---|---|---|---|
| `approvals` | §5 H2, §7.5 | Ask mode: `cargo test --quiet` held and approved once; held again and approved with "always allow"; a new session; another user; `cargo publish --dry-run` | Nothing runs before a decision; one approval runs the command once; the store holds one user-scoped `ShellPrefix [cargo, test]` rule pinned to `bash`'s declaration, with `harness.rule.created`; the new session runs with no held call and `harness.rule.matched`; bob's first call and `cargo publish` are held |
| `plan_mode` | §7.4, §11 `live_plan_mode_writes_nothing` | A fix-the-bug task in Plan mode, then the person approves `exit_plan_mode` | The workspace tree hash is unchanged; every edit or shell call is refused; the todo list is non-empty; `exit_plan_mode` is held; after approval it runs and the mode store records Ask, set by `PlanApproval`, with `harness.mode.changed` |
| `plan_graph` | §5 H4, §7.11 | A three-step plan with an `approve` gate on step 3, on SQLite; the process stops at the gate; a new process denies the gate, rewinds to step 2, approves | `harness.plan.compiled` names `interrupt_before [step:3]`; steps 1 and 2 run and the gate is a held `plan_step_gate`; the graph thread has checkpoints; the gate survives the restart; the denial cancels the plan; the rewind forks the thread and asks again; only step 3 writes; `a.txt` and `b.txt` keep their timestamps |
| `durable_goal` | §5 H1, §7.11, §7.12 | `start_goal` with `until: cargo test --quiet` as a durable run on SQLite; the worker parks on a held write; the process stops; a new process approves the calls | The run waits and nothing changed; the held call survives the restart; the same run completes in the new process; the goal is done with the check passing; the action ledger holds one completed record per approved `write_file` and `bash`; each approved `bash` began once; `cargo test --quiet` passes |
| `team` | §5 H3, §7.10 | The `explore` member answers a question; in Plan, the `general` member is asked to write and run a command | The member's first request carries the delegated request, not the user's message; it reads the code itself; `harness.member.spawned` (local, parent `harness`) and `harness.member.finished` (completed); in Plan the member's `write_file` and `bash` are refused and nothing is written |
| `identity` | §5 H6, §7.14 | `RbacMap::harness_defaults()` with a scope resolver: alice holds `harness:shell`, bob holds nothing; Auto with host commands allowed | alice's command runs; bob's is refused with `harness.rbac.denied:harness:shell`; `who.txt` holds alice's line only; one `harness.bash.begin`, acting for alice |
| `watches_itself` | §5 H7, §7.13 | A one-question run, then a request for four identical reads under `LoopGuard::default()` | `Harness::spend` reports one run, its model calls and tokens, keyed by `urn:adk:agent:harness-walkthrough/harness` and subject alice, with `harness.spend`; the loop turn stops with `harness.loop_detected`, the mode store records ReadOnly set by `SystemTighten`, and `harness.loop.detected` is audited |
| `sandbox` | §7.9 | `ShellIsolation::OsSandbox`: `touch inside.txt`, then `touch` of a file outside the workspace; a prompter that approves sandboxed calls and refuses unsandboxed retries | The workspace file exists and the outside file does not; the outside write fails with the enforcer's hint (`denialSuspected` and `retry_of` on Seatbelt); `harness.bash.begin` records a read-write workspace and no network; every unsandboxed retry is put to the person as an escalation and refused. Skipped where no OS sandbox is available |
| `long_term_memory` | §5 H11, §7.15.2 | Nothing yet: `adk-harness` does not export `FactStore` (§7.15.2 is not merged) | Reported as SKIPPED with the reason; the build script sets `cfg(harness_fact_store)` when `adk-harness` exports `FactStore` |

> **Note:** `durable_goal` fixes the code with `write_file`. `edit_file` requires a `read_file`
> of the file in the same process, so a held `edit_file` approved after a restart is refused.

## Results

macOS with Seatbelt, `adk-harness` at `5846e20c`:

| Scenario | Scripted (`ScriptedLlm`) | Live (`claude-sonnet-5-5`) |
|---|---|---|
| `approvals` | PASS (9/9 checks) | PASS (9/9 checks) |
| `plan_mode` | PASS (9/9 checks) | PASS (9/9 checks) |
| `plan_graph` | PASS (12/12 checks) | PASS (12/12 checks) |
| `durable_goal` | PASS (9/9 checks) | PASS (9/9 checks) |
| `team` | PASS (7/7 checks) | PASS (7/7 checks) |
| `identity` | PASS (4/4 checks) | PASS (4/4 checks) |
| `watches_itself` | PASS (7/7 checks) | PASS (6/6 checks) |
| `sandbox` | PASS (7/7 checks) | PASS (7/7 checks) |
| `long_term_memory` | SKIPPED: adk-harness on this checkout does not export FactStore (spec 3.1 §7.15.2 long-term facts are not merged) | SKIPPED: adk-harness on this checkout does not export FactStore (spec 3.1 §7.15.2 long-term facts are not merged) |

## Under the agent kernel (`--kernel`)

`--kernel` runs the harness as a process of an in-process agent kernel (`adk-kernel`, spec 3.2
§5, §11), built as `adk-rust up` builds it: `Harness::kernel()` over the harness, a durable
worker per queue, the scheduler, SQLite stores in `harness.db`, and the control plane on a socket
and loopback HTTP. The default mode is unchanged.

```bash
cargo run -- --kernel                    # K2, K4 and K5, scripted: no key
cargo run -- --kernel --live             # the same on claude-sonnet-5-5 (ANTHROPIC_API_KEY)
cargo run -- --kernel --live k5_one_job_many_windows
```

| Scenario | Spec 3.2 | What runs | What is checked |
|---|---|---|---|
| `k2_sleep_is_free` | §5 K2, §7.4 | Ten processes asleep on ten kinds of wake set (webhook, queue, file change, cron, approval, finished run, budget, manual, time, and a webhook with an expiry); an hour on a `ManualClock` the run store and the kernel share, in one-minute steps; then past the time wake and the expiry | No model call in the hour; no lease at any ten-minute mark; only the ten sleepers exist, all `Waiting`; `ps` lists all ten with their wake sets, no spend and no model calls, each asleep for the hour; past it, the time wake and the expiry run their sleepers to completion on the model, and the other eight still sleep without a lease |
| `k4_it_proposes_its_own_work` | §5 K4, §7.5 | Ask mode: a turn spawned over the socket calls `propose_followup`; a phone on loopback HTTP approves the item from the inbox; a call the follow-up holds is rejected, as the inbox's operator would | The proposal is on the agenda and stays `proposed` with no process before a decision; the inbox offers approve, edit and reject; once approved the item is done, run as a `Proposed` process whose parent is the proposing turn, completed; `kernel.agenda.proposed` and `kernel.agenda.approved` are audited |
| `k5_one_job_many_windows` | §5 K5, §7.11 | Ask mode: a turn spawned from a terminal on the socket holds its `bash` call; the terminal goes away; a phone on loopback HTTP approves the call from the inbox; a new shell attaches | Nothing runs before the decision; the run resumes and completes with no terminal attached; `marker.txt` holds one line; the new shell sees the `bash` answer in the transcript, nothing held, and the same process completed; the spawn and the attach are `kernel.control` records with transport `socket` |

macOS, `adk-harness` and `adk-kernel` at `39b0ba64`, two live runs:

| Scenario | Scripted (`ScriptedLlm`) | Live (`claude-sonnet-5-5`) |
|---|---|---|
| `k2_sleep_is_free` | PASS (10/10 checks) | PASS (10/10 checks) |
| `k4_it_proposes_its_own_work` | PASS (6/6 checks) | PASS (6/6 checks) |
| `k5_one_job_many_windows` | PASS (9/9 checks) | PASS (9/9 checks) |

## Output

An excerpt of the live run (`cargo run -- --live durable_goal team`), temporary paths shortened
to `$TMPDIR`:

```text
== durable_goal (H1): a goal survives the process
  1. Start the goal as a durable run
      harness: The goal runs as durable run e1c61b9a-3e51-4a08-92d0-b992adec8363 (enqueued).
    the durable run's turn, from the session:
      harness: I'll start by reading the file to understand its current contents.
      harness -> read_file {"path":"src/lib.rs"}
      harness <- read_file: {"content":" 1\t/// Adds two numbers.\n 2\tpub fn add(a: i32, b: i32) -> i32 {\n 3\t a - b\n 4\t}\n 5\t\n 6\t#[cfg(test)]\n 7\tmo...
      harness: Now I'll rewrite the file with `a + b` instead of `a - b`:
      harness -> write_file {"content":"/// Adds two numbers.\npub fn add(a: i32, b: i32) -> i32 {\n a + b\n}\n\n#[cfg(test)]\nmod tests {\n #[test]\n fn add...
      harness <- write_file: held for approval
      harness: Tool confirmation required for 'write_file'. Provide approve/deny decision to continue.
    PASS the run parked on a held call (Waiting, pending [{"args":{"content":"/// Adds two numbers.\npub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\n#[c...
    PASS nothing changed while it waits
  2. The process stops
  3. A new process approves; the run resumes
    PASS the held call survived the restart ([{"args":{"content":"/// Adds two numbers.\npub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\n#[cfg(test)]\nm...
    [alice/s1] approve write_file {"content":"/// Adds two numbers.\npub fn add(a: i32, b: i32) -> i32 {\n a + b\n}\n\n#[cfg(test)]\nmod tests {\n #[test]...
    [alice/s1] approve bash {"command":"cargo test --quiet"}
    the durable run's turns, from the session:
      harness <- write_file: {"bytes_written":173,"message":"Wrote 173 bytes to src/lib.rs","path":"src/lib.rs"}
      harness: Now I'll run `cargo test --quiet` to verify the test passes:
      harness -> bash {"command":"cargo test --quiet"}
      harness <- bash: held for approval
      harness: Tool confirmation required for 'bash'. Provide approve/deny decision to continue.
      harness <- bash: exit_code 0
      harness: The test is already passing! Here's a summary of what was done: 1. **Read** `src/lib.rs` — found that `add` was returning `a - b` (subtraction...
      until_check: Goal check `cargo test --quiet` passed on iteration 1. The goal is done.
    PASS run e1c61b9a-3e51-4a08-92d0-b992adec8363 completed in the new process (Completed, approvals ["write_file", "bash"])
    goal: Some(GoalState { goal: "Make the failing test in src/lib.rs pass: read the file, then rewrite the whole file with write_file so that `add` returns ...
    PASS the goal is done in the same session
    PASS the action ledger holds one completed record per approved write_file (1 records, 1 approvals)
    PASS the action ledger holds one completed record per approved bash (1 records, 1 approvals)
    PASS each approved bash began once (1 begins)
    PASS cargo test --quiet passes
  => PASS (9/9 checks)
```
