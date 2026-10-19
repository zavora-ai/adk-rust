- **Tool error messages stay out of model requests and sessions** (`adk-agent`, `adk-core`):
  a tool that failed with `Err(AdkError)` was answered with `{"error": error.to_string()}`, so
  a message quoting a connection string, an internal host, or an upstream body went to the
  model provider and into session storage. The answer now names only the error's component,
  category, and code (`AdkError::redacted`). The message still reaches `on_tool_error`,
  `ToolOutcome::error`, and traces when `RunConfig::record_payloads` is set, and the agent's
  own timeout and panic messages keep their text.
