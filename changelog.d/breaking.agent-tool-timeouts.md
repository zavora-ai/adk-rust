- **`AgentTool` timeouts and propagated failures are errors with their category**
  (`adk-tool`): a delegation that runs past `AgentTool::timeout` is a `Timeout` tool error in
  every `AgentToolFailureMode`, no longer an `{"error": ...}` result in the default mode, so
  `on_tool_error`, the circuit breaker, retries and the action ledger see the failure. Under
  `AgentToolFailureMode::Propagate` a failed delegate's error keeps its category and code
  instead of becoming `tool.legacy`. The delegate's `session_id()` is its caller's session ID.
  `HeldToolCall` gains a public `name` field, the tool the agent resumes, so struct literals
  must set it.
