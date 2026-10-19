- **Guardrail blocks are structured errors** (`adk-agent`, `adk-guardrail`): an input or output
  guardrail block, including the runner's save-time input screening, ends the run with
  `ErrorComponent::Guardrail`, `ErrorCategory::InvalidInput` (HTTP 400), and code
  `guardrail.input_blocked` or `guardrail.output_blocked` (`INPUT_BLOCKED_CODE`,
  `OUTPUT_BLOCKED_CODE`) instead of `agent.legacy` with category `Internal` (HTTP 500).
  `details.metadata` names the blocking `guardrail`, its `severity`, the `phase`, and every
  `failures` entry. Callers that matched `agent.legacy` or `is_agent()` for a block match the
  new code instead. `GuardrailError` gains a `Blocked { phase, failures }` variant, so an
  exhaustive match on it needs an arm.
