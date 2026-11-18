- **`IntegratedRealtimeRunner` ledgers non-idempotent ADK tool calls and counts them against
  the run budget** (`adk-realtime`): under `tool_governance`, a `ToolEffect::NonIdempotent`
  call is begun in `RunConfig::action_ledger` and completed with its outcome, so a call
  redelivered under its call ID runs once, and each call reserves one call in
  `RunConfig::budget_tracker`; the call past the budget ends the session.
