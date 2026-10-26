- **Background shells** (`adk-harness`, `adk-devtools`, `adk-cli`): `bash` takes
  `run_in_background: true` and returns a `shell_id` at once, decided exactly as the same
  command in the foreground; a call that reaches its timeout moves to the background
  instead of being killed. `shell_output` reads a shell's output and `shell_kill` stops one,
  both class `Read`. A shell that ends is named in the next request's reminder until it is
  read. Every shell dies on a freeze, at its output (64 MiB) or lifetime limit (none in the
  REPL, 30 minutes by default otherwise, 2 hours at most), at session end and on
  `Harness::close`; a `ShellPidRegistry` in `harness.db` lets the next start reap what a
  crash left running. `BashTool::spawn` returns a `RunningCommand`; `/shells` lists the
  session's shells and `/shells kill N` stops one (spec 3.1 §7.29).
