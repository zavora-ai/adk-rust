- **`adk-rust approvals` decides on the running kernel's inbox** (`adk-cli`): with a kernel
  up, `list`, `approve` and `deny` use its approvals inbox under the `apr_…` ids every surface
  shows, and an approval resumes the parked process at once; before, the command wrote a
  decision to `harness.db` that the parked process never saw. Calls an in-process session
  holds are listed beside them, and `harness.db` alone is used when no kernel runs. `--home DIR`
  names another kernel's home.
- **`adk-rust approvals --server URL --token T` parses** (`adk-cli`): the documented forms,
  before or after the subcommand, no longer stop at the terminal's "before a subcommand" check.
- **A goal on the kernel runs as you** (`adk-cli`, `adk-harness`): `adk-rust goal` with
  `adk-rust up` running enqueues an authenticated run, so Auto, the session's mode and standing
  rules apply as they do in process; the banner states the mode, and `--yes` approves the goal's
  held calls through the inbox while the terminal follows it.
  `Harness::with_authenticated_goals` and `HarnessLauncher::with_authenticated_goals` set it.
- **Steering a process started without a session** (`adk-kernel`): `Kernel::steer` and
  `POST /kernel/sessions/run-{pid}/steer` reach the turn of a process spawned without
  `sessionId`, which runs in `run-{pid}`; before, the steer answered `returned`.
