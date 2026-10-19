- **Tool failures reach the model without their message** (`adk-agent`, `adk-core`,
  `adk-rust-macros`): the function response for a tool that returned `Err`, or for a
  before-tool callback that failed, names the error's component, category, and code instead
  of quoting its message; a failed tool is answered
  `{"error": "tool '<name>' failed: <component>.<category> (<code>)"}`
  (`adk_core::tool_error_response`). Call `LlmAgentBuilder::expose_tool_error_messages(true)`
  to send the message as before, or return `Ok(json!({"error": "..."}))` or an `on_tool_error`
  fallback for text the model should read. An error with code `tool.invalid_arguments`
  (`adk_core::TOOL_INVALID_ARGUMENTS_CODE`) keeps its text, and `#[tool]` argument decoding
  errors now use that code with `ErrorCategory::InvalidInput` instead of `AdkError::tool`.
- **`ToolOutcome` has an `error` field** (`adk-core`): `Option<Arc<AdkError>>`, the failed
  call's error, so struct literals must set it. `ToolOutcome` is no longer `UnwindSafe` or
  `RefUnwindSafe`.
- **Realtime tool errors reach the model without their message** (`adk-realtime`):
  `RealtimeAgent`, `RealtimeRunner`, `IntegratedRealtimeRunner`, and `ToolBridgeAdapter` answer
  a failed tool with `tool_error_response`, a failed native handler with
  `{"error": "tool '<name>' failed"}`, and a before-tool callback or plugin error with its
  code. Call `expose_tool_error_messages(true)` on the builder, or
  `ToolBridgeAdapter::with_exposed_error_messages(true)`, to send the full message as before.
