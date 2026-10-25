# harness_openai

The `adk-harness` walkthrough on OpenAI: nine scenarios that show what `HarnessAgent` adds to
an agent loop, from approvals bound to the tool to goals that survive the process (spec 3.1
§5). Every check is on files, stores and audit records, never on what the model says.

The scenarios live in [`examples/harness_walkthrough`](../harness_walkthrough) and are shared with
[`examples/harness_anthropic`](../harness_anthropic); this crate supplies the OpenAI model. Each scenario
runs the same code scripted and live: a scripted run replays, through `adk_model::ScriptedLlm`,
what a model that follows the prompt does; a live run sends the same prompts to `gpt-5.6-terra`.

## Run

```bash
cargo run                                # every scenario, scripted: no key
cp .env.example .env                     # then set OPENAI_API_KEY
cargo run -- --live                      # every scenario on gpt-5.6-terra
cargo run -- --live approvals sandbox    # selected scenarios
cargo run -- --live --model <model-id>   # another OpenAI model (OPENAI_MODEL works too)
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

| Scenario | Scripted (`ScriptedLlm`) | Live (`gpt-5.6-terra`) |
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
cargo run -- --kernel --live             # the same on gpt-5.6-terra (OPENAI_API_KEY)
cargo run -- --kernel --live k5_one_job_many_windows
```

| Scenario | Spec 3.2 | What runs | What is checked |
|---|---|---|---|
| `k2_sleep_is_free` | §5 K2, §7.4 | Ten processes asleep on ten kinds of wake set (webhook, queue, file change, cron, approval, finished run, budget, manual, time, and a webhook with an expiry); an hour on a `ManualClock` the run store and the kernel share, in one-minute steps; then past the time wake and the expiry | No model call in the hour; no lease at any ten-minute mark; only the ten sleepers exist, all `Waiting`; `ps` lists all ten with their wake sets, no spend and no model calls, each asleep for the hour; past it, the time wake and the expiry run their sleepers to completion on the model, and the other eight still sleep without a lease |
| `k4_it_proposes_its_own_work` | §5 K4, §7.5 | Ask mode: a turn spawned over the socket calls `propose_followup`; a phone on loopback HTTP approves the item from the inbox; a call the follow-up holds is rejected, as the inbox's operator would | The proposal is on the agenda and stays `proposed` with no process before a decision; the inbox offers approve, edit and reject; once approved the item is done, run as a `Proposed` process whose parent is the proposing turn, completed; `kernel.agenda.proposed` and `kernel.agenda.approved` are audited |
| `k5_one_job_many_windows` | §5 K5, §7.11 | Ask mode: a turn spawned from a terminal on the socket holds its `bash` call; the terminal goes away; a phone on loopback HTTP approves the call from the inbox; a new shell attaches | Nothing runs before the decision; the run resumes and completes with no terminal attached; `marker.txt` holds one line; the new shell sees the `bash` answer in the transcript, nothing held, and the same process completed; the spawn and the attach are `kernel.control` records with transport `socket` |

macOS, `adk-harness` and `adk-kernel` at `39b0ba64`:

| Scenario | Scripted (`ScriptedLlm`) | Live (`gpt-5.6-terra`) |
|---|---|---|
| `k2_sleep_is_free` | PASS (10/10 checks) | not run |
| `k4_it_proposes_its_own_work` | PASS (6/6 checks) | not run |
| `k5_one_job_many_windows` | PASS (9/9 checks) | not run |

## Output

An excerpt of the live run (`cargo run -- --live approvals plan_mode`), temporary paths shortened
to `$TMPDIR`:

```text
== approvals (H2): approvals bound to the tool
  1. Ask mode holds a host command; approve it once
    [alice/s1] user: Run exactly `cargo test --quiet` in the workspace with the bash tool: no other flags, no pipes, no redirections, no `cd`. Then tell me i...
      harness -> bash {"command":"cargo test --quiet","timeout_secs":120}
      harness <- bash: held for approval
      harness: Tool confirmation required for 'bash'. Provide approve/deny decision to continue.
    PASS Ask holds `cargo test --quiet` before anything runs
    [alice/s1] approve once: bash {"command":"cargo test --quiet","timeout_secs":120}
    [alice/s1] resume
      harness <- bash: exit_code 0
      harness: `cargo test --quiet` completed successfully: 1 test passed, 0 failed.
    PASS approved once, it runs once
  2. "Always allow" stores a ShellPrefix rule
    [alice/s2] user: Run exactly `cargo test --quiet` in the workspace with the bash tool: no other flags, no pipes, no redirections, no `cd`. Then tell me i...
      harness -> bash {"command":"cargo test --quiet","timeout_secs":120}
      harness <- bash: held for approval
      harness: Tool confirmation required for 'bash'. Provide approve/deny decision to continue.
    PASS the held call offers the rule `bash: cargo test …`
    [alice/s2] approve and always allow "bash: cargo test …": bash {"command":"cargo test --quiet","timeout_secs":120}
    [alice/s2] resume
      harness <- bash: exit_code 0
      harness: `cargo test --quiet` passed successfully: 1 test passed, 0 failed.
    PASS alice has one user-scoped rule, ShellPrefix [cargo, test], pinned to bash's declaration (1 rules)
    PASS harness.rule.created is audited
  3. A new session runs `cargo test --quiet` with zero prompts
    [alice/s3] user: Run exactly `cargo test --quiet` in the workspace with the bash tool: no other flags, no pipes, no redirections, no `cd`. Then tell me i...
      harness -> bash {"command":"cargo test --quiet","timeout_secs":120}
      harness <- bash: exit_code 0
      harness: Tests pass.
    PASS no call held, `cargo test --quiet` ran (1 bash calls, 0 held)
    PASS harness.rule.matched names the rule
  4. Rules belong to their user
    [bob/s1] user: Run exactly `cargo test --quiet` in the workspace with the bash tool: no other flags, no pipes, no redirections, no `cd`. Then tell me in ...
      harness -> bash {"command":"cargo test --quiet","timeout_secs":120}
      harness <- bash: held for approval
      harness: Tool confirmation required for 'bash'. Provide approve/deny decision to continue.
    PASS bob's first `cargo test --quiet` is held
  5. `cargo publish` still asks
    [alice/s4] user: Run exactly `cargo publish --dry-run` in the workspace with the bash tool and tell me what it printed.
      harness -> bash {"command":"cargo publish --dry-run","timeout_secs":120}
      harness <- bash: held for approval
      harness: Tool confirmation required for 'bash'. Provide approve/deny decision to continue.
    PASS `cargo publish --dry-run` is held, not run (pending: [{"args":{"command":"cargo publish --dry-run","timeout_secs":120},"tool":"bash"}])
    [alice/s4] deny: bash {"command":"cargo publish --dry-run","timeout_secs":120}
  => PASS (9/9 checks)
```
