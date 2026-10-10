- **Tool retries follow the declared effect** (`adk-core`, `adk-agent`): a
  `RetryBudget` retried every failed call, including one that timed out after its side
  effect had happened, so a payment could be charged once per retry. `LlmAgent` and
  `CodeActAgent` now retry only a tool whose new `Tool::effect()` is `ReadOnly` or
  `Idempotent`, and only after a retryable error (`AdkError::is_retryable()`) or a
  timeout. A tool that declares nothing is `NonIdempotent` and runs once; declare
  `ToolEffect::Idempotent` to keep retrying it. The delay doubles per retry up to the new
  `RetryBudget::max_delay`, with jitter, and each attempt gets a fresh tool context so a
  retried attempt's state delta and escalation are discarded. A `RetryBudget` struct
  literal must add `max_delay`; `RetryBudget::new` sets it.
- **`AgentTool` delegations ignore the parent's `tool_timeout`** (`adk-core`,
  `adk-agent`, `adk-tool`): a delegation was cut at the parent's five-minute default
  regardless of its own configuration. The new `Tool::timeout_override()` replaces the
  agent's timeout per tool, and `AgentTool` returns its `timeout` setting, so a
  delegation without one runs until it finishes or the run is cancelled. Set
  `AgentTool::timeout` to bound it.
