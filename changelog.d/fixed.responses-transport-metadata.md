- **Responses HTTP errors keep their status, code and retry hint** (`adk-model`): an
  `OpenAIResponsesClient` 4xx response whose body was not an OpenAI `{"error": ...}` object
  failed with `model.openai_responses.parse` (`Internal`, not retried), and `Retry-After` was
  never read. Every non-2xx response now maps by its HTTP status, quotes up to 512 bytes of a
  non-JSON body (or states that the body could not be read), records an identifier-like error
  `code` as `provider_error_code` and an identifier-like `x-request-id` as
  `ErrorDetails::request_id`, and carries the `Retry-After` delay, which the retry loop honours
  up to `RetryConfig::max_delay`. In Open Responses mode a `{"detail": ..., "code": ...}` body
  is read as the error.
- **Responses SSE framing with `max` reasoning and in Open Responses mode** (`adk-model`): a
  streamed response without a `Content-Type` header, or framed with CR-only line endings, is
  read as SSE instead of being mangled.
- **`retry::parse_retry_after` accepts HTTP-dates** (`adk-model`): all three RFC 9110 date
  formats yield the time remaining, where they returned `None`, and values too large to
  represent saturate, so the retry loop waits `RetryConfig::max_delay`. The OpenAI-compatible,
  Groq, DeepSeek, Azure AI Inference, and OpenAI Responses clients and the Responses WebSocket
  handshake read `Retry-After` through it.
