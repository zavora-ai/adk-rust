- **`RealtimeAgent` records non-idempotent calls in the action ledger and counts them
  against the run budget** (`adk-realtime`, `adk-core`): a `ToolEffect::NonIdempotent` call
  is begun in `RunConfig::action_ledger` after authorization and completed with its outcome, so
  a call the provider issues twice runs once and a timed-out call is answered `outcome_unknown`
  instead of running again; every call counts against `RunBudget::max_tool_calls`, and the call
  past it is answered `not run` and ends the session with the budget error. The ledger step is
  now public as `adk_core::begin_ledgered_call`, shared by `LlmAgent`, `CodeActAgent` and
  `RealtimeAgent`. A realtime tool's context exposes the run configuration
  (`ToolContext::run_config`), so a realtime tool reads the run's egress policy.
