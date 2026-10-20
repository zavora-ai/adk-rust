# Autonomous goal loop — `/goal` with durable checkpointing

Codex/Hermes-style **goal mode** as an ADK-Rust example: set a goal + a
**verifiable success condition** (a shell command that must exit 0), and the
agent loops **plan → act → verify**, self-correcting from the check's output,
until the condition passes or the iteration budget is reached.

The agent is the `CodingAgent` preset of [`adk-harness`](../../adk-harness) in ask mode:
every edit and command passes the harness policy and waits for approval. The demo runs
unattended, so an `ApproveAndLog` confirmation handler approves each call and prints it.
`adk-rust goal "…" --until "…"` is the CLI form, with approvals at the terminal and the goal
kept in `harness.db`.

It's durable: after **every** iteration the goal state is atomically
checkpointed to disk (`.adk/goal.json`), so a restart can `resume` from where it
left off — and a completed goal is recognized as done.

## Run

```bash
cargo run --manifest-path examples/coding_goal/Cargo.toml
```

Requires `GOOGLE_API_KEY` (Gemini 3, default) — or `CODING_PROVIDER=openai` with
`OPENAI_API_KEY`. Override the model with `CODING_MODEL`.

## What the demo does

1. Seeds a workspace with a buggy `stats.py` (`mean` forgets to divide) + a
   failing `test_stats.py`.
2. Runs the goal loop until `python3 test_stats.py` exits 0, checkpointing each
   iteration.
3. Prints the persisted checkpoint.
4. Re-runs with `resume = true` to show a completed goal is a no-op (simulating a
   restart).
5. Independently verifies the result.

```text
━━ iteration 1/5 ━━
  🔧 bash        (python3 test_stats.py → fails)
  🔧 read_file   stats.py
  🔧 edit_file   return sum(xs)  →  return sum(xs) / len(xs)
  🔧 bash        (python3 test_stats.py → ok)
  ✅ goal met after 1 iteration(s)

══ durable checkpoint (.adk/goal.json) ══
{ "goal": "...", "until": "python3 test_stats.py", "iteration": 1, "status": "done", ... }

══ resume (simulating a restart) ══
  goal already complete (per checkpoint); nothing to do.

══ verification ══
  ✅ PASS python3 test_stats.py (exit Some(0)) after 1 iteration(s)
```

## CLI equivalent

The same capability ships in the CLI, with `--resume`:

```bash
adk-rust goal "make all tests pass" --until "cargo test" --max-iters 8
adk-rust goal "..."                  --until "cargo test" --resume   # continue after a restart
```
