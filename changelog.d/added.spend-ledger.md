- **One spend ledger for model calls and payments** (`adk-core`, `adk-session`,
  `adk-runner`): `SpendLedger` reserves an amount against every `SpendLimits` cap that
  covers a `SpendKey` (org, agent, vendor; UTC day, month, or lifetime), commits the
  actual amount, or releases the hold; unsettled holds expire after a TTL
  (`DEFAULT_RESERVATION_TTL`, 15 minutes). `InMemorySpendLedger` ships in `adk-core`, and
  `SqliteSpendLedger` and `PostgresSpendLedger` in `adk-session` serialize concurrent
  reservations so a cap is never overshot. With `RunConfig::spend_ledger` or
  `Runner::builder().spend_ledger(...)` set, each model call reserves its upper-bound cost
  under the app, the acting agent's `AgentId`, and the vendor read from the model id, and
  commits the reported `UsageMetadata::cost` under the provider the response reports
  (`LlmResponse::provider`); a refused reservation fails the call with
  `spend.limit_exceeded`. A call to an unpriced model that reports no cost commits the
  `LlmSpendEstimate` set with `Runner::with_llm_spend_estimate` (or
  `RunConfig::llm_spend_estimate`).
