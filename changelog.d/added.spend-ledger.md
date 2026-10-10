- **One spend ledger for model calls and payments** (`adk-core`, `adk-session`,
  `adk-runner`): `SpendLedger` reserves an amount against every `SpendLimits` cap that
  covers a `SpendKey` (org, agent, vendor; UTC day, month, or lifetime), commits the
  actual amount, or releases the hold; unsettled holds expire after a TTL.
  `InMemorySpendLedger` ships in `adk-core`, and `SqliteSpendLedger` and
  `PostgresSpendLedger` in `adk-session` serialize concurrent reservations so a cap is
  never overshot. With `RunConfig::spend_ledger` set, the runner reserves an estimate
  before each model call (`Runner::with_llm_spend_estimate`) and commits the reported
  `UsageMetadata::cost`, keyed by app, agent, and vendor; a refused reservation fails the
  call with `spend.limit_exceeded`.
