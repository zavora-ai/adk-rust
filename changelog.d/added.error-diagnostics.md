- **Content-free error helpers** (`adk-core`): `AdkError::diagnostics()` displays an error
  by everything but its message, for log lines and span fields; `adk_core::upstream_excerpt`
  and `UPSTREAM_EXCERPT_BYTES` bound upstream text quoted in a message; and
  `adk_core::json_error_summary` describes a JSON decoding error without the value it
  quotes.
- **Log privacy example** (`examples/log_privacy`): provider 400s, retries, and stream
  errors echoing a secret, and the same secret in a tool error, guardrail denial, callback
  error, and toolset error, checked against every log line and span field with
  `record_payloads` off and on, plus a live OpenAI 400.
