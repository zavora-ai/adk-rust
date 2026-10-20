- **Anthropic transport metadata** (`adk-anthropic`): `Error::RateLimit` and
  `Error::ServiceUnavailable` include `status_code`, and `Error::Timeout` includes
  an optional `status_code`. `Error::Api` and `Error::InternalServer` include
  `retry_after`. Existing constructors keep their signatures; direct variant
  construction supplies the new fields, and matching can use `..`.
- **Anthropic byte-idle limits**: `Anthropic::with_http_timeouts` and
  `AnthropicClient::with_timeouts` set connection and byte-inactivity bounds
  without cutting off active streams. HTTP-date retry delays and original
  502/504/529 statuses reach model errors; 529 remains a rate-limit SDK error.
  Transport timeouts retain the `Timeout` category, and SSE comments are ignored.
