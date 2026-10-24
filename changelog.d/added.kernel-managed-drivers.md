- **A managed definition's drivers on its tenant's kernel** (`adk-kernel`, `adk-harness`; spec
  3.2 §7.7, §7.8, §7.13, K-I13, K-I15): `TenantKernelBuilder::drivers` gives a tenant its own
  `DriverSupervisor`. Each driver a definition's partition mounts is proposed in the tenant's
  approval inbox as a `kernel:driver.mount` request (`DRIVER_MOUNT_TOOL_NAME`) showing the
  command line, partitions, effect classes, sandbox and secret references, and is mounted only
  when a person approves it (`TenantKernel::apply_driver_decisions`, every scheduler tick while
  the kernel runs; `TenantKernelBuilder::driver_approvers` sets who approves). Its tools'
  declarations are pinned at their first listing. `PartitionHost::drivers` carries the
  supervisor, so `HarnessResolver` mounts each partition's own drivers and gives the
  coordinator the definition's other partitions' drivers as crossings. A changed manifest is
  asked again. Each tenant's drivers run under a home of the tenant's own, and `ManagedKernels`
  refuses a tenant whose driver supervisor or driver store another tenant holds.
