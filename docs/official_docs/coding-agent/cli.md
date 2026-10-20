# Coding Agent CLI

The `adk-rust` CLI ships three native coding commands. All run a real agent in a
**confined workspace** and resolve their model the way `chat` does: `--provider`, `--model`
and `--api-key`, then the provider's environment variable (`GOOGLE_API_KEY`/`GEMINI_API_KEY`,
`OPENAI_API_KEY`, …), the OS credential store, and the provider saved by first-run setup.
Setup prompts only on a terminal; without one, a command with no configured provider fails
and names the flags to pass, so it never blocks on a prompt. `--instruction` adds project
guidance to the coding prompt.

> **Important:** `code` (unless `--read-only`), `goal`, and `ultracode` give the agent the
> `bash` tool. It runs model-written commands on the host with a cleared environment; the
> file tools are confined to `--dir`, but `bash` is not an OS sandbox. See
> [Dev tools](devtools.md#sandboxing-model).

## `code` — one-shot task

```bash
adk-rust code "make the failing test pass"
adk-rust code --dir ./project "add a /health route"
adk-rust code --read-only "explain how auth works"   # no writes / no shell
```

The agent explores, edits, runs commands, and streams its work (tool calls,
results, text) plus the final plan.

## `goal` — autonomous goal mode (durable, resumable)

Give a goal **and a verifiable success
condition** (`--until` is a shell command that must exit 0). The agent loops
**plan → act → verify**, self-correcting from the check's output, until it passes
or the iteration budget is reached.

```bash
adk-rust goal "make all tests pass" --until "cargo test" --max-iters 8
adk-rust goal "port utils.py to type hints" --until "mypy utils.py" --dir ./svc
```

**Durable & resumable.** After every iteration the goal state is atomically
checkpointed to `<dir>/.adk/goal.json` (override with `--state <path>`). If a run
is interrupted, continue it:

```bash
adk-rust goal "…" --until "cargo test" --resume
```

`--resume` recognizes a completed goal as a no-op, and continues a `running` or
budget-`exhausted` run from where it left off.

| Flag | Meaning |
|------|---------|
| `--until <cmd>` | Success condition: exit 0 = goal met (**required**). |
| `--dir <path>` | Workspace directory (default `.`). |
| `--max-iters <n>` | Iteration budget (default 8). |
| `--state <path>` | Checkpoint file (default `<dir>/.adk/goal.json`). |
| `--resume` | Resume from the saved checkpoint. |

> The success condition is what makes goal mode safe and effective — pick
> something verifiable (a test suite, a type-checker, a build) so the agent has a
> clear, machine-checkable definition of "done."

## `ultracode` — parallel ultra-review

Implement the task, then fan
out to **parallel specialist reviewers** (correctness, edge-cases, style),
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
flags. Without them the commands use Gemini's default model, `gemini-3.7-flash`. For OpenAI: `--provider openai --model gpt-5.6-terra` (with `OPENAI_API_KEY`).

Next: [Workflows →](workflows.md)
