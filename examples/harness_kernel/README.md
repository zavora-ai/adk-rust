# harness_kernel

Scenarios for the agent kernel (`adk-kernel`, spec 3.2 §5, §11). Every pass condition is
asserted on stores, processes, files and audit records, never on model prose.

## Scripted scenarios

No keys needed. Each scenario boots a real kernel — a durable worker per queue, the
scheduler, SQLite stores in `harness.db` and the control plane — over a harness on a
scripted model, built with `Harness::kernel` as `adk-rust up` builds it, and prints a
main-versus-branch table. `main` has no kernel, so its cells read `not run, expected: …`, stating what `main`
does instead.

```bash
cargo run                          # every scripted scenario
cargo run -- k5_one_job_many_windows
```

| Scenario | What passes |
|---|---|
| `k1_leave_it_running` | K1 without service installation, on a manual clock the run store and the kernel share: a nightly audit is scheduled two hours ahead, the kernel stops, the clock moves eight hours on while it is down, and the kernel boots again; nothing ran while it was down, the next boot runs the missed firing exactly once, the run completes, the file is written once, `ps` shows the process `Completed`, and the replica's signed audit log verifies across the restart once it signs a checkpoint |
| `k4_it_proposes_its_own_work` | K4: in Ask, a turn's `propose_followup` call puts a `proposed` item on the agenda and in the inbox (approve, edit, reject) and nothing runs for a second; a phone on loopback HTTP approves it; it runs as a `Proposed` process whose parent is the proposing turn, and `kernel.agenda.proposed` and `.approved` are audited. In Auto at 23:00, on a manual clock, the policy schedules it, nothing starts inside the 22:00–07:00 quiet hours, and it runs at 07:01 |
| `k8_a_message_preempts_background_work` | K8: three background jobs hold their first model call at a quota of three; a typed question runs and completes at once; the youngest job is preempted by it, parks at its next checkpoint, is requeued (`kernel.process.suspended`, `.requeued`) and completes with its read run once; every job completes; sampling shows at most three admitted processes |
| `k5_one_job_many_windows` | K5: a turn spawned from a terminal on the socket holds its `bash` call; the terminal goes away; a phone on loopback HTTP approves the call from the inbox; the run resumes and the command runs once with no terminal attached; a new shell attaches and sees the same process, completed, and the call's answer in the transcript; the spawn and the attach are recorded as `kernel.control` with their transport |
| `k6_drivers_you_can_trust` | K6 on a real stdio MCP server (this binary, `serve-notes-driver`): mounted as an approval and run under the OS sandbox; its `read` tool runs without asking and its declarations are pinned; killed, it restarts with backoff (`crashed`, `restarting`, `running` events); a redeclared tool is `needsReapproval` and its call is held for a person; `reapprove` restores it, the held call completes, and every step is in the audit log |

## Live scenarios

The live scenarios run the `adk-rust up` binary in a private home and reach it over its
control plane, as any surface does.

```bash
cargo build -p adk-cli --bin adk-rust          # from the workspace root
cp .env.example .env                           # add the keys you have
ADK_RUST_BIN=../../target/debug/adk-rust cargo run -- --live
cargo run -- --live --provider anthropic --bin ../../target/debug/adk-rust
```

`HARNESS_ENV_FILE=/path/to/.env` loads keys from another file. The providers are
`anthropic` (`claude-sonnet-5-5`) and `gemini` (`gemini-3.5-flash-lite`); one without a key
is skipped.

| Scenario | What passes |
|---|---|
| `live_k5_approve_from_another_process` | `up --mode ask`: the model's `bash` call is held; the client that started the turn disconnects; `adk-rust approvals --server … approve` in another process approves it over HTTP; the turn completes, the command ran exactly once, and a new client attaching sees the call in the transcript |
| `live_k4_a_proposal_waits_for_approval` | `up --mode ask`: the model calls `propose_followup`; the item waits `proposed` and nothing runs; `adk-rust agenda approve <prefix>` in another process approves it; the follow-up runs as a `Proposed` process whose parent is the proposing turn and completes, any call it holds in Ask rejected by the scenario as an operator would; it answers in its own session |
| `live_k3_file_change_wakes` | `up --mode ask`: a process spawned asleep on a `notes.md` file wake makes no model call; saving the file wakes it within two seconds; the turn reads the file with `read_file` and answers with its codeword |
| `live_k6_supervised_driver` | `up --mode ask`: `adk-rust drivers mount --yes` shows the notes driver's command line, partitions and classes and mounts it; the model uses its `read` tool without approval; the driver, killed, restarts with backoff; a redeclared tool is listed `needsReapproval` and its call is held; `adk-rust drivers reapprove` restores it |
| `live_parked_question_after_restart` | `up --mode ask`: the model's `send_note` is approved, the server asks before sending and the question waits in the inbox; the kernel stops and starts again; the answer completes the turn, the note is sent exactly once, and nothing is asked again |
| `live_k10_proc` | Six weeks of history ending yesterday are seeded into the kernel home's ledger, two of them late finishes; `up --mode ask` runs a live turn, whose intent, tool call, turn and process rows reach the ledger with no prompt, file or answer in them; the pass of the day that ended records an observation, which the live model words as a neutral question; with long-term memory on, the observation waits in the agenda as a question and no process runs for it, `adk-rust agenda schedule` on it is refused, and `adk-rust agenda approve` confirms it, remembering its statement as a known fact whose source is the person; `adk-rust proc ledger --export` writes every row as the JSONL `cargo adk eval --ledger` reads (spec 3.2 §7.10) |
