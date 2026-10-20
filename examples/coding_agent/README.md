# Coding Agent example

Runs the ADK-Rust **`CodingAgent`** preset of [`adk-harness`](../../adk-harness)
(spec 3.1 §7.19) — the [`adk-devtools`](../../adk-devtools) file tools, a governed
`bash`, todos and `exit_plan_mode` — against real tasks. File tools are confined to the
workspace; `bash` runs on the host. The preset runs in **ask** mode: every edit and
command passes the harness policy and waits for approval. The example runs unattended,
so a small `ApproveAndLog` confirmation handler approves each call and prints it
(`✅ approved …`), the way `adk-rust code --yes` does.

## Run

```bash
# Multi-language demo (Rust, Python, JavaScript) in a temp workspace:
cargo run --manifest-path examples/coding_agent/Cargo.toml

# Multi-turn build — grow a medium program over several turns in one session:
cargo run --manifest-path examples/coding_agent/Cargo.toml -- multiturn

# Scenario tour — real agent, increasing complexity, each independently verified:
cargo run --manifest-path examples/coding_agent/Cargo.toml -- tour

# A single scenario (hello | multifile | fixtest | debug | refactor):
cargo run --manifest-path examples/coding_agent/Cargo.toml -- fixtest

# A single task in a directory you choose:
cargo run --manifest-path examples/coding_agent/Cargo.toml -- ./some/dir "make the failing test pass"
```

## The multi-turn build

`multiturn` keeps **one agent, one runner, one session** and sends a sequence of
follow-up requests, so the agent builds on its own prior work (the session
history persists across turns). It grows a Python `todo` CLI from nothing into a
working, tested program:

1. `add` / `list` (JSON-persisted tasks)
2. `done <index>` (with `[x]`/`[ ]` markers)
3. `rm <index>`
4. robust error handling + usage/exit codes
5. a `test_todo.py` (subprocess tests) — run until it passes

Then the example **independently verifies** the result by running
`python3 test_todo.py`. A typical run produces ~140 lines across `todo.py` +
`test_todo.py`, all tests green — and you can watch later turns `read_file` the
existing code and `edit_file` it rather than starting over.

## The scenario tour

`tour` runs a progression of tasks of increasing complexity. Each one sets up a
fixture, lets the **real** agent work, then **independently verifies** the result
by running the produced code — no mocks:

| Scenario | Demonstrates |
|----------|--------------|
| `hello` | Write a single file (no execution). |
| `multifile` | Create a 2-file program and run it (`40 + 2 → 42`). |
| `fixtest` | Read a failing test, find the bug, fix it, re-run until green. |
| `debug` | Run a crashing script, read the traceback, fix the runtime error. |
| `refactor` | Rename a symbol across files and confirm it still runs. |

Example (the `fixtest` read–modify–test loop, abridged):

```text
  🔧 bash({"command":"python3 test_calc.py"})        ↩ exit 1 (AssertionError)
  🔧 read_file({"path":"calc.py"})                   ↩ "return a - b"
  🔧 edit_file({"old_string":"return a - b","new_string":"return a + b", …})
  🔧 bash({"command":"python3 test_calc.py"})        ↩ exit 0, "tests passed"
  ✅ verify: tests exit Some(0), stdout "tests passed"
```

Requires `GOOGLE_API_KEY` (default, Gemini 3) — or set `CODING_PROVIDER=openai`
with `OPENAI_API_KEY`. Override the model with `CODING_MODEL`.

| Env var | Default | Notes |
|---------|---------|-------|
| `CODING_PROVIDER` | `gemini` | `gemini` or `openai` |
| `CODING_MODEL` | `gemini-3.1-flash-lite` (Gemini) / `gpt-5-mini` (OpenAI) | Any model id |
| `GOOGLE_API_KEY` / `GEMINI_API_KEY` | — | For Gemini |
| `OPENAI_API_KEY` | — | For OpenAI |

## What you'll see

For each task the agent streams its work — tool calls (`🔧`), approvals (`✅`), tool
results (`↩`), and final text (`🤖`) — then prints the plan the session holds:

```text
  🔧 write_todos({"todos":[{"content":"Create add.rs …","status":"in_progress"}, …]})
  🔧 write_file({"content":"fn add(a: i32, b: i32) -> i32 { a + b } …","path":"add.rs"})
  ✅ approved write_file({"content":"fn add(a: i32, b: i32) -> i32 { a + b } …","path":"add.rs"})
  🔧 bash({"command":"rustc add.rs -o add"})
  🔧 bash({"command":"./add"})
  ↩  {"exit_code":0,"stdout":"5\n", …}
  🤖 The file add.rs was created, compiled, and executed. The output of ./add is 5.
  📋 plan:
     ✓ Create add.rs …
     ✓ Compile add.rs with rustc.
     ✓ Run the executable and report output.
```

## CLI equivalent

The same harness ships in the CLI, with approvals at the terminal:

```bash
adk-rust code "make the failing test pass"            # current dir, ask mode
adk-rust code --dir ./project "add a /health route"
adk-rust code --mode plan "explain how auth works"    # reads only; plans
adk-rust code --yes "make the failing test pass"      # approve every held call
```

See `docs/design/spec-3.1-harness-agent.md` for the design.
