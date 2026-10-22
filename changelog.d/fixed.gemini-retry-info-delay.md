- **Gemini waits the `retryDelay` a 429 asks for** (`adk-model`): without a `Retry-After` header,
  `GeminiModel` takes the retry hint from the `retryDelay` of the error's `google.rpc.RetryInfo`
  detail (`"12s"`, `"1.5s"`) on AI Studio and Vertex AI, so a call on a low quota waits for the
  quota instead of failing after three short backoffs. A delay above
  `RetryConfig::max_server_delay` is not waited; the error carries it on `AdkError::retry`.
