# Coding Agent example

Runs the ADK-Rust **`CodingAgent`** — the [`adk-devtools`](../../adk-devtools)
toolset (read/write/edit/glob/grep/bash) plus the harness in
[`adk-agent`](../../adk-agent) (feature `coding`) — against real tasks. The agent
plans with `write_todos`, edits files, and runs commands in a **sandboxed
workspace**.

## Run

```bash
# Multi-language demo (Rust, Python, JavaScript) in a temp workspace:
cargo run --manifest-path examples/coding_agent/Cargo.toml

# A single task in a directory you choose:
cargo run --manifest-path examples/coding_agent/Cargo.toml -- ./some/dir "make the failing test pass"
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

For each task the agent streams its work — tool calls (`🔧`), tool results
(`↩`), and final text (`🤖`) — then prints the completed plan:

```text
  🔧 write_todos({"todos":[{"content":"Create add.rs …","status":"in_progress"}, …]})
  🔧 write_file({"content":"fn add(a: i32, b: i32) -> i32 { a + b } …","path":"add.rs"})
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

The same capability ships in the CLI:

```bash
adk-rust code "make the failing test pass"          # current dir
adk-rust code --dir ./project "add a /health route"
adk-rust code --read-only "explain how auth works"  # no writes / no shell
```

See `docs/design/coding-agent.md` for the overall design.
