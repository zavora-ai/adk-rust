- **`UsageMetadata::provider_usage` is the provider's usage object as received**
  (`adk-model`): OpenAI Responses, Groq, Ollama (the final line's metric fields) and Gemini
  Interactions now set it, and Gemini, Anthropic and DeepSeek keep fields their typed clients
  do not model, such as Gemini `toolUsePromptTokenCount`, `trafficType` and `serviceTier`, or
  Anthropic `service_tier`, `inference_geo` and the `cache_creation` breakdown. A streamed
  Anthropic message keeps the `message_start` usage with the `message_delta` fields applied
  over it.
- **Typed usage keeps the raw object** (`adk-anthropic`, `adk-gemini`): `Usage::raw`,
  `MessageDeltaUsage::raw`, `UsageMetadata::raw` and `interactions::Usage::raw` hold the usage
  object exactly as the API sent it; `None` for a value built in code.
