- **Run budgets** (`adk-core`, `adk-runner`, `adk-agent`): `RunBudget` limits model calls,
  total tokens, cost, wall time and tool calls; `RunConfig::budget` or
  `Runner::builder().budget(...)` applies it to every run with fresh counters.
  `BudgetTracker` is shared through `RunConfig::budget_tracker` with transfer targets,
  workflow sub-agents, team members and agent tools. Limits are checked before every model
  call and tool batch, and a call held for confirmation counts once against the tool-call
  limit, not again when it resumes (`BudgetTracker::release_tool_calls`). A run that reaches
  a limit ends with a persisted event explaining why (`budget_exceeded_event`) and a
  `ResourceExhausted` error with a `budget.*` code. Under a cost cap, a response with no
  known cost stops the run (`budget.cost_unknown`) unless
  `RunBudget::allow_unpriced_models()` is set. `generate_with_budget` meters a model call
  made outside an agent against a tracker.
