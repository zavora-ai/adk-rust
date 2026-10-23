- **The kernel's managed form** (`adk-kernel` feature `managed`, `adk-harness`; spec 3.2
  §7.13, §10 M1; Experimental, as `adk-managed` is): one kernel per tenant on the managed
  runtime.
  - `KernelDef` travels under the `adk.kernel` metadata key of a `ManagedAgentDef`, beside
    `adk.harness`: wake sets (cron, webhook and queue `Wake`s with `overlap: skip | queue`),
    a `ProactivityPolicy`, `DriverManifest`s and partitions. `from_agent_def` validates it and
    refuses what a tenant may not ask for — an unsandboxed driver, pinned declaration digests,
    a plain credential, the `interactive` queue, a wake on the host's files.
  - `TenantKernel` runs one kernel over a tenant's `ManagedStores`: each hosted definition as
    a target, its wake sets as triggers, the definitions' proactivity policy with the
    consolidation tick off unless a definition turns it on (§13 Q1), and the tenant's approval
    service waking a parked process. It opens no control plane of its own.
  - `KernelResolver` (`TenantKernel::resolver`) is the `AgentResolver` the tenant's runtime is
    built with: it hosts each definition carrying a kernel block on the tenant's kernel, its
    agent built by the operator's resolver, such as `HarnessResolver`.
  - `ManagedKernels` holds a host's tenants and refuses a tenant whose stores another holds.
  - `HarnessAgentBuilder::agenda_handle` puts a harness's `propose_followup` items on a
    tenant kernel's agenda; `adk-harness`'s `managed` feature turns on `adk-kernel/managed`
    when `kernel` is on.
