- **A kernel says what it runs and what it would silently not do** (`adk-kernel`,
  `adk-website`): `Kernel::inventory()` lists the kernel's targets, standing triggers (each
  with whether an agent or workflow serves its target), wake sources, the partitions it files
  signals in and its stores, with a warning for a trigger whose target nothing serves
  (`kernel.trigger.unserved`), a signal trigger in a partition the kernel files no signals in
  (`kernel.trigger.signal_unfiled`), a store held in memory (`kernel.store.in_memory`) and a
  kernel with nothing to run (`kernel.idle`). The kernel logs it at start and serves it at
  `GET /api/kernel/inventory` to the `operator` scope (`ControlClient::inventory`); a host or
  kit adds its own warnings with `KernelBuilder::inventory_warning`. With `charter`,
  `KernelBuilder::signals(store, partition)` registers the partitions a kernel files signals in
  and `Kernel::signal_sink(partition)` returns the partition's sink on the kernel's agenda, wake
  router, bus, clock and audit trail; `adk-website serve` files its edge's signals through it
  (`edge::adapters::KernelSignals::attach` replaces `wake`).
