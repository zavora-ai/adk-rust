- **`adk-rust` starts the harness REPL** (`adk-cli`, `harness` feature, on by default): with
  no subcommand the binary runs the spec 3.1 `HarnessAgent` in the current directory in ask
  mode — file tools confined to the workspace, `bash` on the host behind per-call approval,
  session todos, `AGENTS.md` loaded after a one-time trust prompt, progressive skills. It
  refuses the home directory and `/` unless `--dir` names them. Approval prompts offer
  `[y] allow once  [a] always allow…  [n] deny (reason)  [e] edit args  [?] details`; `[a]`
  remembers a standing rule bound to the OS user and the tool's declaration, and a denial
  ends the turn. Slash commands: `/mode`, `/sandbox`, `/todos`, `/rules`, `/approvals`,
  `/plan`, `/cost`, `/team`, `/session`, `/skills`, `/memory`. The REPL, `code`, `goal`,
  `ultracode` and `serve` share `--dir`, `--mode`, `--yes`, `--session`/`--continue`/
  `--ephemeral`, `--sandbox`, `--allow-unsandboxed`, `--fail-if-unavailable`, `--trust`,
  `--pack` and `--member`, read after `.adk/harness.toml` (which can only tighten) and
  `<config dir>/adk-rust/harness.toml`. Sessions, held calls, durable runs, modes and
  standing rules share `<config dir>/adk-rust/harness.db`; a session keeps the workspace it
  started in. `adk-rust chat` is the plain agent, unchanged.
