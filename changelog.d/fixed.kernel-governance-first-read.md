- **A kernel reads its mirrored kill switch before it takes work** (`adk-kernel`):
  `Kernel::start` reads the governance store once, for at most
  `KernelConfig::GOVERNANCE_FIRST_READ`, before it starts the workers, so a process spawned
  as soon as `start` returns is no longer refused as "last read never". A store that cannot be
  read still refuses work until the mirror reads it. A replica that starts while a replicated
  freeze is in force keeps the processes the freeze parked, and the kernel wakes them once its
  control allows work again, including after a stale store is read again.
