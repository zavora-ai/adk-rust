- **Persistent `chat` sessions** (`adk-cli`, `persist` feature, on by default): `adk-rust
  chat` keeps its conversations in SQLite at `<config dir>/adk-rust/harness.db`, created
  readable by its owner only, under the OS user name. `--session <id>` continues that session
  or creates it, `--continue` continues the most recent one, and `--ephemeral` keeps the
  conversation in memory. The banner shows the session id.
