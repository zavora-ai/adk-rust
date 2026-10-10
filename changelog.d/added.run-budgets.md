- **Run budgets** (`adk-core`, `adk-runner`, `adk-agent`): `RunBudget` limits
  model calls, total tokens, cost, wall time and tool calls;
  `RunnerConfigBuilder::budget` applies it to every run with fresh counters.
  `BudgetTracker` is shared through `RunConfig::budget_tracker` with transfer
  targets, workflow sub-agents and agent tools. Limits are checked before every
  model call and tool dispatch; a run that reaches one ends with a persisted
  event explaining why and a `ResourceExhausted` error. Under a cost cap, an
  unpriced model stops the run unless `RunBudget::allow_unpriced_models()` is
  set. `generate_with_budget` meters model calls made outside `LlmAgent`.
