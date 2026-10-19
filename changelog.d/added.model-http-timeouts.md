- **`HttpTimeouts` for model clients** (`adk-model`): `OpenAICompatible`, `OpenAIClient` (both
  routes), `AzureOpenAIClient`, `OpenAIResponsesClient`, `AzureAIClient`, `GroqClient`,
  `DeepSeekClient`, `OpenRouterClient`, and `OllamaModel` bound connecting (30 seconds) and
  every wait for data — the response headers, then each gap between body chunks (10 minutes)
  — by default, so a stalled stream fails instead of hanging the run; `with_timeouts` sets
  both bounds. `BedrockClient` bounds each stream event at 10 minutes by default, and its
  `with_timeouts` also sets the SDK client's connect and read timeouts. A stalled Chat
  Completions, Groq, DeepSeek, Azure AI, Ollama, or Bedrock stream fails with
  `ErrorCategory::Timeout`; OpenAI Responses and OpenRouter report it as a retryable
  `Unavailable` error.
