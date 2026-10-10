- **`ErrorCategory::ResourceExhausted`** (`adk-core`): a new variant for
  exhausted budgets, HTTP 429 and not retryable. Exhaustive matches on
  `ErrorCategory` add an arm.
- **`RunConfig` gains `budget` and `budget_tracker`** (`adk-core`): struct
  literals of `RunConfig` add both fields or use `..Default::default()`.
- **Team budget errors are `ResourceExhausted`** (`adk-agent`):
  `agent.team.budget_exceeded` was `RateLimited`, which retry policies treated
  as retryable. Model-call, token, cost, tool-call and wall-time violations now
  surface with their `budget.*` code.
- **Model events stop embedding the request twice** (`adk-agent`): terminal
  model events carry one `llm_request` copy bounded by
  `RunConfig::trace_payload_max_bytes` (full only with the `record-payloads`
  feature and `RunConfig::record_payloads`) and no
  `gcp.vertex.agent.llm_request` metadata, so stored event size no longer grows
  with conversation history.
