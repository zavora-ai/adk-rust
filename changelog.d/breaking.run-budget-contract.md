- **Team budget errors are `ResourceExhausted`** (`adk-agent`):
  `agent.team.budget_exceeded` was `RateLimited`, which retry policies treated
  as retryable. Model-call, token, cost, tool-call, and wall-time limits of a
  `TeamBudget` are enforced through the run's `BudgetTracker` and surface with
  their `budget.*` code; event, delegation, and handoff limits keep
  `agent.team.budget_exceeded`.
- **Team cost caps fail closed on unknown cost** (`adk-agent`):
  `TeamBudget::max_cost_microusd` enforces against the priced usage every
  provider now reports, and a model response with no known cost stops the team
  unless the new `TeamBudget::allow_unpriced_models` (`allowUnpricedModels`) is
  set. A `TeamBudget` struct literal without `..TeamBudget::default()` must add
  the field.
- **Model events stop embedding the request twice** (`adk-agent`): terminal
  model events carry one `llm_request` copy bounded by
  `RunConfig::trace_payload_max_bytes` (full only with the `record-payloads`
  feature and `RunConfig::record_payloads`) and no
  `gcp.vertex.agent.llm_request` provider metadata, so stored event size no
  longer grows with conversation history.
