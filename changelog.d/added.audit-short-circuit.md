- **Short-circuited tool calls are audited** (`adk-agent`): when a `BeforeToolCallback`, a
  run-wide `before_tool` plugin, or an enhanced plugin's `ShortCircuit` answers a tool call,
  `LlmAgent` records a `tool_call` record with action `tool.short_circuit`, outcome
  `not_run`, the argument digest, and `toolName`, `agentName`, `answeredBy`,
  `functionCallId`, `sessionId`, and the tool's `toolSource` and `declarationDigest`, through
  the run's audit trail. Runs without an audit
  trail are unchanged; under `AuditFailureMode::Block` a short-circuit that cannot be
  recorded ends the run with `audit.record_failed`.
