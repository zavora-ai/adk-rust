- **The durable worker reads the run store's clock** (`adk-server`, `adk-kernel`): `DurableWorker`
  pruned finished runs past `retain_finished` against the wall clock, while the run store stamps
  their times on its own clock, so a store whose clock trailed the wall clock by more than the
  retention window lost runs that had just finished. `DurableWorker::with_clock` sets the clock
  that the worker reads for pruning, idle waits on `not_before`, hand-back deadlines and approval
  expiry. The kernel passes `KernelConfig::clock` to its workers.
