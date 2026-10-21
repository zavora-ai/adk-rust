# Coding Agent CLI

> **Note:** these commands run on the [harness](../harness/index.md); [The harness in your
> terminal](../harness/cli.md) is the full reference.

The `adk-rust` CLI ships three native coding commands. All run the harness of spec 3.1 — the
same agent as the `adk-rust` REPL — in a **confined workspace**, and resolve their model the way `chat` does: `--provider`, `--model`
and `--api-key`, then the provider's environment variable (`GOOGLE_API_KEY`/`GEMINI_API_KEY`,
`OPENAI_API_KEY`, …), the OS credential store, and the provider saved by first-run setup.
Setup prompts only on a terminal; without one, a command with no configured provider fails
and names the flags to pass, so it never blocks on a prompt. `--instruction` adds project
guidance to the coding prompt.

> **Important:** the commands run in **ask** mode: every file edit and every `bash` command
> waits for your approval at the terminal (`[y]`, `[a] always allow…`, `[n]`, `[e]`, `[?]`).
> `bash` runs model-written commands on the host with a cleared environment unless
> `--sandbox` puts it in the OS sandbox; the file tools are confined to `--dir`. Unattended
> scripts pass `--yes`, or choose a mode with `--mode`. Without a terminal and without
> `--yes`, calls are held and the command exits with status 3. See
> [The harness in your terminal](../harness/cli.md) for every flag, and
> [Dev tools](devtools.md#sandboxing-model).

## `code` — one-shot task

```bash
adk-rust code "make the failing test pass"
adk-rust code --dir ./project "add a /health route"
adk-rust code --mode plan "explain how auth works"   # reads only, proposes a plan
adk-rust code --yes "make the failing test pass"     # approve every held call
```

The agent explores, edits, runs commands after your approval, and streams its work (tool
calls, approvals, results, text) plus the session's todo list. The session is kept in
`harness.db`; `--session <id>` continues it. `--read-only` remains as a hidden alias of
`--mode read-only`.

## `goal` — autonomous goal mode (durable, resumable)

Give a goal **and a verifiable success
condition** (`--until` is a shell command that must exit 0). The agent loops
**plan → act → verify**, self-correcting from the check's output, until it passes
or the iteration budget is reached.

```bash
adk-rust goal "make all tests pass" --until "cargo test" --max-iters 8
adk-rust goal "port utils.py to type hints" --until "mypy utils.py" --dir ./svc
```

**Durable & resumable.** The goal runs on the harness goal engine (`Harness::start_goal`) as a
durable run in `harness.db`: a turn toward the goal, then the check, until it passes. A call
held without a terminal parks the run; after `adk-rust approvals approve`, `--resume`
continues the run at that call:

```bash
adk-rust goal "…" --until "cargo test" --resume
```

`--resume` continues the latest unfinished goal in this workspace — running, parked, or
budget-exhausted when `--max-iters` is above the iterations it used — in its session and with
its conversation; with no unfinished goal it starts a new one. See
[Goals](../harness/cli.md#goals) for the exit codes.

| Flag | Meaning |
|------|---------|
| `--until <cmd>` | Success condition: exit 0 = goal met (**required**). |
| `--dir <path>` | Workspace directory (default `.`). |
| `--max-iters <n>` | Iteration budget (default 8). |
| `--check-timeout <secs>` | How long one run of `--until` may take before it counts as failed (default 600). |
| `--resume` | Resume the latest unfinished goal in this workspace. |
| `--yes`, `--mode`, `--session` | The harness flags; goals need `ask` or `auto` mode. |

> The success condition is what makes goal mode safe and effective — pick
> something verifiable (a test suite, a type-checker, a build) so the agent has a
> clear, machine-checkable definition of "done."

## `ultracode` — parallel ultra-review

Implement the task in your mode, then fan
out to **parallel specialist reviewers** (correctness, edge-cases, style) that run in
read-only mode,
synthesize their verdicts, and **revise** until they approve — built on
[`adk-graph`](workflows.md).

```bash
adk-rust ultracode "add input validation to the parser"
adk-rust ultracode --dir ./project --max-rounds 3 "implement a retry wrapper"
```

| Flag | Meaning |
|------|---------|
| `--dir <path>` | Workspace directory (default `.`). |
| `--max-rounds <n>` | Maximum review → revise rounds (default 2). |

You'll see each reviewer's verdict, the synthesized decision per round, and the
revise steps, ending in `finalize`.

## Picking a model

All three commands accept the global `--model` / `--provider` / `--api-key`
flags. Without them the commands use the provider saved by first-run setup. For OpenAI:
`--provider openai --model gpt-5.6-terra` (with `OPENAI_API_KEY`).

Next: [Workflows →](workflows.md)
