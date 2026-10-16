- **Tool error messages stay out of model requests and sessions** (`adk-agent`, `adk-core`):
  a tool that failed with `Err(AdkError)` was answered with `{"error": error.to_string()}`, so
  a message quoting a connection string, internal host, or upstream body went to the model
  provider and into session storage, and into the action ledger's failure record. The call is
  now answered by `adk_core::tool_error_response`, which names the error's component,
  category, and code (`AdkError::redacted`). Before- and after-tool callback errors and
  action-ledger store errors are described the same way. The message is still passed to
  `on_tool_error`, carried as `ToolOutcome::error`, and traced when
  `RunConfig::record_payloads` is set. Errors with code `TOOL_INVALID_ARGUMENTS_CODE` and the
  agent's own timeout and panic messages keep their text. An enhanced plugin's
  `after_tool_call` now receives the tool outcome through `ctx.tool_outcome()`, and
  `adk-retry-reflect` recognizes refusals and non-idempotent timeouts from its message, so a
  redacted scope denial is still not suggested for retry.
