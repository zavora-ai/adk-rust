- **Default tool failure text no longer includes the error message** (`adk-agent`,
  `adk-core`, `adk-rust-macros`, `adk-tool`): the function response for a failed tool call,
  or for a before-tool callback that returned `Err`, names the error's component, category,
  and code instead of quoting its message. Call
  `LlmAgentBuilder::expose_tool_error_messages(true)` to send the full message as before, or
  return `Ok(json!({"error": "..."}))` or an `on_tool_error` fallback for text the model should
  read. `ToolOutcome` gains a public `error: Option<Arc<AdkError>>` field, so struct literals
  must set it. `#[tool]` argument deserialization errors are now `ErrorCategory::InvalidInput`
  with code `tool.invalid_arguments` (`adk_core::TOOL_INVALID_ARGUMENTS_CODE`), whose message
  the model keeps.
