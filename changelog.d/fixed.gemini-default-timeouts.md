- **Gemini API-key clients time out a stalled call** (`adk-gemini`, `adk-model`):
  `GeminiModel::new`, `Gemini::with_model` and `GeminiBuilder` now give the Studio client
  a 30-second connect and a 10-minute read timeout, the workspace's `HttpTimeouts`
  defaults, so a call that gets no response fails as a retryable `Timeout` instead of
  holding the run. A builder passed to `with_http_client` keeps its own timeouts.
