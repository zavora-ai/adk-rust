- **Responses streams survive unknown event types** (`adk-model`): a stream event whose
  `type` the client does not recognize is skipped instead of ending the turn, and an empty
  terminal output no longer erases text already streamed. Standard-mode streams reach the
  SDK unmodified.
- **Responses HTTP errors keep their diagnostics** (`adk-model`): a non-JSON error body is
  quoted (up to 512 bytes) in the message, an unreadable body is reported as such, and an
  identifier-like `x-request-id` populates `ErrorDetails::request_id`. Oversized
  `Retry-After` delays are capped at `RetryConfig::max_delay` instead of disabling retry.
- **`retry::parse_retry_after` accepts HTTP-dates** (`adk-model`): all three RFC 9110 date
  formats yield the time remaining, and delays too large to represent saturate. Every
  provider that reads `Retry-After` through it now honours date values.
- **Native function tools keep an explicit `strict`** (`adk-model`): a native declaration
  that falls back to a function tool sends its own boolean `strict` rather than `false`.
