- **Team budgets count each model call once** (`adk-agent`): partial streaming
  chunks, which repeat cumulative usage on Gemini, no longer add events,
  requests, tokens or cost, so the default 128-event blackboard budget no
  longer trips on streaming speakers. `maxCostMicrousd` now enforces against
  priced usage, and members stop before the model or tool call that would start
  past a team limit.
- **Team runtimes no longer retain every invocation** (`adk-agent`): finished
  invocations beyond the 64 most recent are evicted; receipts stay in session
  state.
