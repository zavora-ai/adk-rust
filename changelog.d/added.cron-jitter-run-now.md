- **Cron jitter and run now** (`adk-core`, `adk-server`, `adk-kernel`, `adk-cli`):
  `schedule::Jitter` delays each fire by a fixed per-schedule share of the gap to the next one
  (up to `percent`, at most `max`, always under half the gap; `Jitter::default()` is 15 percent,
  at most nine minutes). `DurableScheduler::with_jitter`, `CronState::with_jitter` and
  `KernelConfig::with_cron_jitter` turn it on; the fire keeps its scheduled time as its
  idempotency key and watermark and waits out the offset as `not_before`. `POST /cron/{id}/run`
  and `Kernel::run_now` (`POST /api/kernel/processes/{pid}/run-now`,
  `POST /api/kernel/triggers/{name}/run-now`, `adk-rust run-now`) run a schedule now: refused
  while frozen, subject to the job's concurrency policy or the kernel's admission, recorded in
  the audit trail, and without moving the schedule.
