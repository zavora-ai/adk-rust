- **`CharterKit` installs a site's jobs on a kernel** (`adk-kernel`, `adk-website`):
  `adk_kernel::charter::CharterKit` binds each `SiteProcessKind` the product runs to its own
  target and input (`job`, `job_on`), and `install(builder)` registers the bound kinds' standing
  triggers in the back-office partition, the KPI tracker as the kernel's `kernel.kpi` process
  (`KernelBuilder::kpi_tracker`, hourly by default) and the signal sink (`KernelBuilder::signals`);
  a kind left unbound is a `charter.job_unbound` warning in the kernel's inventory, with the
  reason `skip` gave. `KernelBuilder::workflow` adds a workflow executor beside the others
  (`workflows` still replaces them). `adk-website serve` binds its jobs through the kit
  (`jobs::processes` is the binding table, `SiteProcess::input` replacing `trigger`), so its
  KPI tracker shows in `ps` and waits out a freeze instead of ticking in a loop of its own, and
  re-pauses a breached guardrail's jobs at start.
