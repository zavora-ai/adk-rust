# Background sub-agents

A lead agent on the agent kernel starts [background sub-agents](../../docs/official_docs/agents/background-subagents.md),
keeps working, and is told when they finish. Every agent runs live on Anthropic.

| Command | What it shows |
|---|---|
| `cargo run -- scenario` | The lead starts `alpha` and `beta` and calls `note_progress` while they run; `alpha`'s finish wakes it; it sends `alpha` a follow-up and cancels `beta`; `gamma` holds `publish_summary`, shows `waitingForHuman`, and finishes once the call is approved in the inbox. Ends with a requirement, result and evidence table |
| `cargo run -- crash --home DIR` | Starts `delta`, whose `fetch_archive` call takes two minutes, prints `READY_FOR_KILL pid=…`, and waits |
| `cargo run -- resume --home DIR` | Reopens `DIR` after a `kill -9`: the kernel recovers `delta`, it finishes, and the lead is told once |

## Run

```bash
cp .env.example .env   # set ANTHROPIC_API_KEY
cargo run -- scenario

cargo run -- crash --home /tmp/subagents-crash &
# wait for READY_FOR_KILL, then:
kill -9 <pid>
cargo run -- resume --home /tmp/subagents-crash
```

`HOLD_FOR_PS=<seconds>` pauses the scenario while `alpha` and `beta` run and while `gamma`
waits, printing `PS_NOW … home=DIR`, so `adk-rust ps --home DIR` can be run from another
terminal; `crash` serves the same socket until it is killed.

## What it wires

| Part | Where |
|---|---|
| Kernel stores and sessions | SQLite under the home directory (`kernel.db`, `sessions.db`) |
| Sub-agent host | `KernelBuilder::subagents` with `researcher`, `archivist` and `publisher` targets and a default budget of 8 model calls and 8 tool calls |
| Tools | `SubagentTools` over a `SubagentHandle` on the lead, beside `note_progress`, `fetch_archive` and `publish_summary` (the sub-agents' tool ceiling is the lead's tools) |
| Approvals | `StoreApprovalService` over the kernel's run store; the scenario approves `gamma`'s held call from the inbox |
