- **`UsageMetadata::provider_usage` is the provider's usage object as received** (`adk-model`,
  `adk-anthropic`, `adk-gemini`): OpenAI Responses, Groq, Ollama (the final line's metric fields)
  and Gemini Interactions now set it, and Gemini, Anthropic and DeepSeek fill it from the response
  instead of re-serializing their typed usage, so fields the typed clients do not model survive,
  such as Gemini `toolUsePromptTokenCount`, `trafficType` and `serviceTier`, or Anthropic
  `service_tier`, `inference_geo` and the `cache_creation` breakdown. For a streamed Anthropic
  message it is the `message_start` usage with the `message_delta` fields applied over it. The
  typed clients keep the same object in `adk_anthropic::Usage::raw`, `MessageDeltaUsage::raw`,
  `adk_gemini::UsageMetadata::raw` and `interactions::Usage::raw`.
- **`usage_accounting` example** (`examples/usage_accounting`): drives `LlmAgent` and `Runner`
  with the OpenAI Responses, Anthropic, Gemini, Gemini Interactions, DeepSeek, Groq and Ollama
  clients against a scripted local upstream whose usage objects carry `null` or missing counts,
  and checks the session's usage, the tool's run count and the run budget's counters; live
  scenarios run when a provider key is set.
