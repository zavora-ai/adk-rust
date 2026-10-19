- **Gemini errors are classified by status, not message text** (`adk-model`): AI Studio and
  Vertex AI failures take their category from the HTTP status, or, without one, from the RPC
  status code or the typed transport failure, instead of from words in the error message, and
  `upstream_status` is only the status the transport reported. HTTP 500, 502, and other 5xx
  statuses are `Unavailable` and retried, 504 is `Timeout` and retried, and 403 is
  `Forbidden`; a 500, 502 or 403 was `Internal` and not retried. Both backends record
  `details.metadata["provider_error_code"]`: the first `ErrorInfo.reason`, then the error
  object's string `code`, `type`, or `status`, then, on Vertex AI, the RPC status name.
