- **`adk-rust serve` serves the harness** (`adk-cli`): the served agent is the harness in plan
  mode (`--mode` changes it) with the approval inbox under `/api/approvals` and background
  runs under `/api/runs`; a held call is decided over HTTP and runs when an empty-message run
  resumes the session. A served harness in plan mode lets the model read the workspace.
  `--agent chat` serves the plain chat agent as before.
