- **Tool effect contract** (`adk-core`, `adk-tool`, `adk-rust-macros`): `ToolEffect`
  (`ReadOnly`, `Idempotent`, `NonIdempotent`), `Tool::effect()` (`ReadOnly` when
  `is_read_only()` is true, `NonIdempotent` otherwise), `Tool::timeout_override()`, and
  `ToolContext::idempotency_key()`, which identifies one tool call across every attempt:
  `{app}/{user}/{session}/{invocation}/{call_id}` by default, and
  `{app}/{user}/{session}/codeact/{run_id}/{iteration}/{call_id}` for a `CodeActAgent` call,
  which keeps its key when replayed from a checkpoint.
  `#[tool(idempotent)]` and `FunctionTool::with_effect` declare an effect, and the
  `adk-auth`, `adk-tool`, and team delegation wrappers forward it.
  `RetryBudget::with_max_delay` and `RetryBudget::backoff_delay` set and compute the retry
  backoff.
