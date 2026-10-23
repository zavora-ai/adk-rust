- **`adk-rust up` and the kernel commands** (`adk-cli`, spec 3.2 §8.1, §8.2): `up` runs
  the harness as the agent kernel in the foreground — what `serve` builds plus a worker
  per queue, the scheduler, the process table in `harness.db` or the `--store` backend,
  a signed audit log per replica and the control plane on the kernel home's socket and
  under `/api/kernel`; `POST /api/admin/freeze` parks running processes. `attach` (and
  `adk-rust` with no subcommand while a kernel runs) opens a REPL whose turns the kernel
  runs, answering held calls inline; `ps`, `top`, `kill`, `suspend`, `resume`,
  `freeze`, `unfreeze` and `events` reach it over the socket or `--server`; `goal`
  enqueues on its background queue. A `[kernel]` table in `harness.toml` sets the home,
  bind and per-queue concurrency, which a workspace file may only lower.
  `HarnessLauncher::with_kernel` and `HarnessLauncher::shared` give library users the
  resident form, and the `harness` template's `cargo run -- up` runs it.
- **`harness-kernel` feature** (`adk-rust`, spec 3.2 §8.2): `adk_rust::kernel` re-exports
  `adk-kernel` and turns on `adk-harness`'s `kernel` glue; it is part of `full`.
