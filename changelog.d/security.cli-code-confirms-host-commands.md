- **`code`, `goal` and `ultracode` ask before they edit or run commands** (`adk-cli`): the
  three commands run on the harness in ask mode, so host `bash` and file edits wait for
  approval at the terminal instead of running unconfirmed. Unattended scripts pass `--yes`
  (which never overrides a policy denial and never approves an unsandboxed retry without
  `--allow-unsandboxed`) or choose a mode with `--mode`. Without a terminal and without
  `--yes`, `code` holds the calls, prints the session id and exits with status 3; decide them
  with `adk-rust approvals approve` and continue the session. `ultracode` reviewers run in
  read-only mode. `code --read-only` is a hidden alias of `--mode read-only`, and
  `goal --state` writes a JSON mirror of the goal kept in the session.
