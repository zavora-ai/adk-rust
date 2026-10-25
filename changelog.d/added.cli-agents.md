- **Your own agents on `adk-rust up`** (`adk-cli`): the kernel keeps agents a person adds in
  its agent store (SQLite in `harness.db`, PostgreSQL, Redis, or memory with `--ephemeral`) and
  builds each one with the primary agent's composition, on its own model when its key resolves
  (the scripted model under `--model-script`). `/agent list|show|new|edit|pause|resume|remove`
  joins the REPL's commands, so the full-screen terminal and a kernel's command routes run it
  too; a change prints its preview and applies only with `--confirm <code>`. `adk-rust agents`
  runs the same verbs from a script, asking on the terminal before a change applies.
