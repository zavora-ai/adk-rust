- **Output guardrails on realtime text** (`adk-realtime` feature `guardrails`):
  `IntegratedRealtimeRunnerBuilder::output_guardrails` and
  `RealtimeAgentBuilder::output_guardrails` screen each completed turn with a
  `GuardrailSet`, as `LlmAgent` screens final text: a transform replaces the persisted
  text, a block interrupts the response, persists nothing of the turn and reports
  `RealtimeError::OutputBlocked` (the agent ends the run with the guardrail's error).
