- **Signal wakes in managed definitions** (`adk-kernel`): a `KernelDef` wake set takes a
  `signal` wake, hearing a kind in its own partition or one the block declares. With the
  `charter` feature, `TenantKernelBuilder::signal_store` and `TenantKernel::signal_sink` file a
  tenant's signals and wake its kernel across restarts; a signal-started run stays
  unauthenticated and starts with the definition's input. A definition with a signal wake is
  refused on a tenant without a signal store, and `at` and `budgetReplenished` wakes are refused
  with what to use instead.
