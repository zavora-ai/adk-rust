- **Realtime native handlers carry a tool origin, and approvals naming no agent are refused**
  (`adk-realtime`, `adk-core`; #1210): a call to a native handler registered on a
  `RealtimeRunner` (or an `IntegratedRealtimeRunner`) carries `REALTIME_HANDLER_ORIGIN`
  (`realtime`) and a digest of its `ToolDefinition` (`ToolDefinition::handler_origin`), so an
  approval of it no longer covers a handler redefined since, or a `Tool` of the same name.
  `RunConfigBuilder::tool_approval` refuses a fingerprint that names no acting agent, which
  could never authorize a governed call, and `tool_confirmation_fingerprints` logs a binding to
  one; `ToolConfirmationRequest::fingerprint_agent` reads the agent a fingerprint names.
