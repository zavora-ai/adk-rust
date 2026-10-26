- **Website catalogue, served public tools and draft joining** (`adk-harness`, feature
  `website`): the catalogue lists `edge_read_only` (reversible, pre-authorisable at "act,
  reversible only", A-I11), `experiment_start`, `experiment_report` and `lead_handoff`;
  `website_public_served` builds a public agent over `ServedPublicTools`, such as the AWP
  edge's; `LadderCompiler::with_tool_origins` pins a driver tool's standing rules to its
  origin; `SiteApprovals::record_approval` refuses a vote that cannot count; `content_draft`,
  `content_edit` and `translation_draft` take a `draft` to join, so one release carries several
  items (spec 3.5 §7.8.2, §7.10, §7.11, A-I23).
- **Trigger cron sources and the control address** (`adk-kernel`): a standing cron trigger
  fires through a watermarked `CronWakeSource` the kernel registers per expression, unless a
  registered source reports the expression through the new `WakeSource::cron_expression`;
  `Kernel::control_addr` returns the control plane's bound HTTP address (spec 3.5 §7.11, §8.2).
- **The jobs' tools** (`adk-website`): `experiments::experiment_tools` and
  `alarms::handoff_tool` implement the catalogue's experiment and handoff tools; the `social`,
  `nurture`, `handoff`, `experiments` and `protect` jobs bind the tools they act with (spec 3.5
  §7.20).
