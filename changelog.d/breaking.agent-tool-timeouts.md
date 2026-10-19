- **`AgentTool` timeouts and propagated failures are errors with their category**
  (`adk-tool`): a delegation that runs past `AgentTool::timeout` is a tool error with
  `ErrorCategory::Timeout` in every `AgentToolFailureMode`, no longer an `{"error": ...}`
  result in the default `ReturnErrorObject` mode, so `on_tool_error` and the circuit breaker
  see the failure. Under `AgentToolFailureMode::Propagate` a failed delegate's error
  keeps its category and code instead of becoming `tool.legacy`. The delegate's
  `session_id()` is its caller's session ID instead of an `agent-tool-{uuid}` ID.
