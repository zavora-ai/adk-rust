- **Per-unit daily caps and partition token caps** (`adk-kernel`, `adk-harness`): the kernel's
  daily totals per process and partition hold model calls, total tokens and usage per
  `UsageUnit` beside cost. `SpendDelta` and `RunUsage` carry them (`with_model_calls`,
  `with_total_tokens`, `with_units`), the harness reports each session's tokens and units from
  its run budgets, and `ProcessStore::usage` returns `UsageTotals` on every backend (in-memory,
  SQLite, PostgreSQL, Redis, MongoDB, Firestore, Neo4j; a required method, with the
  `usage_sums_counters_by_day_and_scope` conformance case and a new `adk_kernel_usage` table on
  SQLite and PostgreSQL). `Quotas::daily_caps` (`dailyCaps`, `with_daily_cap`) caps a
  partition's daily usage of a unit: admission holds background and proactive work at a reached
  cap until `BudgetReplenished`. The daily proactive budget's `max_model_calls` and
  `max_total_tokens` are now per-partition daily caps, enforced by admission, the consolidation
  tick gate and agenda proposals. `SpendDelta` and `RunUsage` are no longer `Copy`.
