- **A standing trigger no longer queues runs behind an unfinished one** (`adk-kernel`): a
  trigger's run that waited for a person's approval left every later firing queued behind it,
  without bound. `Trigger` takes a `WakeOverlap` (`with_overlap`), the type a managed wake set
  already used: `skip` (the default) starts nothing while a process the trigger started is
  unfinished, `queueOne` (new) keeps one process waiting behind it and joins later firings to
  it, `queue` queues every firing. A run now follows it too: under `skip`, `Kernel::run_now` of
  a trigger with an unfinished process is refused with `kernel.invalid_state`. The router
  publishes `WakeOutcome::Skipped` and `WakeOutcome::Coalesced`, the audit trail records them,
  `adk-rust events` shows them, and `adk-rust ps` counts them on the unfinished process
  (`kernel.skipped`, `kernel.coalesced`). `WakeRouter::with_clock` is public, and the kernel
  inventory lists each trigger's `overlap`.
- **`MissedFirePolicy` holds for a freeze and for held work** (`adk-kernel`): a kernel frozen
  across two occurrences of a `fireOnce` trigger started two runs on `unfreeze`, since every
  occurrence the freeze held was routed as on time. The router now ranks a cron tick when it
  routes it, behind the later occurrences already due, and passes over the ones its policy
  does not pick (`WakeOutcome::Missed`, so `WakeOutcome::pid` returns `Option<&str>`); under
  `skip`, a tick more than a minute past its release is missed. A cron occurrence that comes
  due while admission holds the trigger's run joins it once the trigger keeps as many runs
  waiting as its policy fires.
