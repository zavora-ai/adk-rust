- **Responses generation failures remain recoverable** (`adk-model`): `stream_timeout`
  is a retryable timeout, and unknown or absent failure codes are retryable `Unavailable`.
  Known authentication, quota, context and input-policy failures stay non-retryable.
  Flat and nested error events share this classification; `response.failed` does not
  require successful-response fields. Unrecognized provider codes and messages remain
  excluded from diagnostics and error source chains.
