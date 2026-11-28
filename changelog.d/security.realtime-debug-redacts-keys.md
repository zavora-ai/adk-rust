- **Realtime API keys stay out of `Debug` output** (`adk-realtime`): `OpenAIRealtimeModel`,
  `GeminiLiveBackend` (and so `GeminiRealtimeModel`) and the OpenAI WebRTC ephemeral-key
  response print `[REDACTED]` in place of the key or credentials, as the `adk-model` provider
  configs do.
