- **OpenRouter upstream errors name their cause** (`adk-model`): an error OpenRouter relays
  from an upstream provider carries a generic message ("Provider returned error") and the
  provider's explanation in `metadata.raw`. The `AdkError` message now appends the provider
  name and that explanation, cut to 512 bytes, for HTTP and stream errors alike:
  `Provider returned error (Google AI Studio): … is temporarily rate-limited upstream`.
