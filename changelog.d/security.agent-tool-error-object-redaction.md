- **`AgentTool` failure objects leave out the delegate's error message** (`adk-tool`): in the
  default `AgentToolFailureMode::ReturnErrorObject`, a failed delegation returned
  `{"error": "agent execution failed: <full error>"}`, so a message quoting a connection
  string or an upstream response body reached the calling agent's model provider and session,
  bypassing tool-error redaction. The result now names the error's component, category, and
  code (`AdkError::redacted`); `AgentTool::expose_error_messages(true)` restores the message.
  The sub-agent failure log line records the same form.
