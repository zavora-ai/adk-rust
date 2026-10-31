- **A governance freeze ends pending approval waits** (`adk-core`, `adk-agent`): a call
  waiting for its `ToolConfirmationHandler` is refused as soon as the run's
  `GovernanceControl` is frozen (locally, by the replicated store, or by a fail-closed
  mirror going stale), whatever `tool_confirmation_timeout` says. Before, the run kept
  waiting, and an answer given after an unfreeze ran the call on a policy evaluated
  before the freeze. The refusal is recorded as `denied` by `governance`, and the inbox
  request and store entry the wait left open are withdrawn, so a later answer runs
  neither the call nor an identical one. `LlmAgent`, `CodeActAgent`, the realtime
  runners, graph nodes, harness teams and `confirm_tool_call` callers (kernel drivers,
  payments) all wait through this path. A held call whose resume `LlmAgent` answers
  `not run` because of a freeze has its stored decision spent. Supersedes #1005, by
  dux-web.
