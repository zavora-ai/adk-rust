- **Provider error codes** (`adk-model`): Anthropic, Chat Completions (`OpenAIClient`,
  `OpenAICompatible` and its presets, `AzureOpenAIClient`), Azure AI Inference, DeepSeek, and
  OpenRouter failures record the provider's own code in
  `AdkError::details.metadata["provider_error_code"]`, so callers can tell provider responses
  apart without parsing message text; error frames inside Chat Completions, Groq, DeepSeek,
  and Azure AI streams record it too. The code is the first Google `ErrorInfo.reason`, then
  the error object's string `code`, `type`, or `status`, so a `null` OpenAI `code` falls back
  to `type`, and array-wrapped bodies (`[{"error": {...}}]`) are read. OpenRouter keeps its
  raw `code` metadata entry alongside the new key.
