- **Sub-second tool timeouts report their limit** (`adk-agent`): a timeout under one second
  was reported as "timed out after 0 seconds" by `LlmAgent` and "0s" by `CodeActAgent`; the
  limit is now written as a duration, such as `50ms` or `300s`, and the failure is an
  `ErrorCategory::Timeout` error with code `tool.timeout` in `ToolOutcome::error`.
