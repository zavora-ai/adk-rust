- **Tool result guardrails** (`adk-guardrail`, `adk-agent`, `adk-core`):
  `ToolGuardrail::validate_result` sees a tool's result before the model and the session do,
  and can revise or withhold it (`ToolResultGuardrailResult`, `ToolGuardrailSet::evaluate_result`).
  `LlmAgent` runs it through `tool_guardrails` on every result it sends the model.
  `ResultTextGuardrail` applies any content guardrail, such as `PiiRedactor`, to every text
  value of a result. `adk_core::ToolResultScreen`, which `ToolGuardrailSet` implements, and
  `result_blocked_response` let the realtime runtimes take the same set. `validate_call` now
  defaults to allowing, so a result-only guardrail implements one method.
- **Low-severity guardrail findings are recorded** (`adk-guardrail`, `adk-agent`): a `Low`
  failure still passes the content, and is now logged at `warn` and recorded on the output
  event under `adk.guardrail.warnings` (`GUARDRAIL_WARNINGS_KEY`).
  `GuardrailExecutor::enforce` returns the screened content and those warnings, or a
  `GuardrailError::Blocked`.
- **`guardrail_coverage` example** (`examples/guardrail_coverage`): output guardrails on text
  beside a tool call, structured block errors, and realtime tool-result screening and error
  redaction, against scripted OpenAI-compatible and OpenAI Realtime servers and live OpenAI.
