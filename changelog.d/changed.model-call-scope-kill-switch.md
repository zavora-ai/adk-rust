- **Metered model calls check the kill switch** (`adk-core`): a `ModelCallScope` built for a
  run carries the run's `GovernanceControl`s (`ModelCallScope::with_governance`) and refuses
  every model or unit-billed call it meters while one is frozen, so compaction summaries, MCP
  sampling and each attempt of a fallback chain stop with `governance.frozen`.
