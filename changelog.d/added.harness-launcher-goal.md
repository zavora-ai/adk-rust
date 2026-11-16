- **`HarnessLauncher` runs goals and a durable kernel** (`adk-cli`): `run` takes
  `goal TEXT --until CMD` with `--mode`, `--sandbox`, `--yes`, `--allow-unsandboxed`,
  `--max-iters`, `--session` and `--resume`; with a kernel answering at the launcher's home the
  goal runs on it and is followed until it ends or holds a call, else in process.
  `with_kernel_stores`, `with_kernel_config` and `with_trigger` give `up` durable stores (a
  mirrored kill switch, refusing any store held in memory), a home, quotas and standing
  triggers. `run_with_args` returns the exit code instead of exiting, and
  `ConsoleIo::set_approve_all` turns on `--yes` for a `ConsolePrompter` built earlier.
