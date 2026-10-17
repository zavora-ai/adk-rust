- **Guardrail blocks are structured errors** (`adk-agent`, `adk-guardrail`): an input or output
  guardrail block, including the runner's save-time input screening, ends the run with
  `ErrorComponent::Guardrail`, `ErrorCategory::InvalidInput` (HTTP 400), and code
  `guardrail.input_blocked` or `guardrail.output_blocked` (`INPUT_BLOCKED_CODE`,
  `OUTPUT_BLOCKED_CODE`) instead of `agent.legacy` with category `Internal` (HTTP 500).
  `details.metadata` names the blocking `guardrail`, its `severity`, the `phase`, and every
  `failures` entry. Callers that matched `agent.legacy` or `is_agent()` for a block match the
  new code instead. `GuardrailError` gains a `Blocked { phase, failures }` variant, so an
  exhaustive match on it needs an arm.
- **Realtime tool and callback errors reach the model redacted** (`adk-realtime`): a failed
  ADK tool is answered with its component, category, and code
  (`adk_core::tool_error_response`), a failed native handler with `tool '<name>' failed`, a
  before-tool callback or plugin error with its code, and an after-tool callback error withholds
  the result (`result_withheld`), on `RealtimeAgent`, `RealtimeRunner`,
  `IntegratedRealtimeRunner`, and `ToolBridgeAdapter`. Call `expose_tool_error_messages(true)`
  on the builder (or `ToolBridgeAdapter::with_exposed_error_messages(true)`) to send the full
  message as before.
