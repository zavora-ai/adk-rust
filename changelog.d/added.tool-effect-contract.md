- **Tool effect contract** (`adk-core`, `adk-tool`, `adk-rust-macros`): `ToolEffect`
  (`ReadOnly`, `Idempotent`, `NonIdempotent`), `Tool::effect()`,
  `Tool::timeout_override()`, and `ToolContext::idempotency_key()`, which identifies one
  tool call across every attempt. `#[tool(idempotent)]` and `FunctionTool::with_effect`
  declare an effect, and the `adk-auth`, `adk-tool`, and team delegation wrappers forward
  it. `RetryBudget::with_max_delay` and `RetryBudget::backoff_delay` set and compute the
  retry backoff.
