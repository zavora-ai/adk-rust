- **Gemini errors use typed response status** (`adk-model`): AI Studio and Vertex AI
  failures are classified from the HTTP status or the RPC status code instead of the
  error message; only a failure with neither is classified as a connection or transport
  failure. HTTP 500, 502, and other 5xx statuses are now
  `Unavailable` and retried, 504 is `Timeout` and retried, and 403 is `Forbidden`. Both
  backends record the same `details.metadata["provider_error_code"]`: the first
  `ErrorInfo.reason`, then the error's string `code`, then its `status`.
