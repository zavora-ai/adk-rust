- **Per-call reasoning effort** (`adk-core`, `adk-model`; spec 3.1 §7.23, §10 C4):
  `GenerateContentConfig` gains `effort: Option<Effort>`, so a struct literal that names every
  field ends with `..Default::default()`. A set value wins over the effort the client was built
  with, for that call only: `AnthropicClient` sends `output_config.effort` with adaptive
  thinking on Claude 4.6 and later (`output_config.effort` alone on Claude Opus 4.5),
  `OpenAIClient`, `OpenAICompatible` and `AzureOpenAIClient` send `reasoning_effort`,
  `OpenAIResponsesClient` sends `reasoning.effort`, `GeminiModel` sends `thinking_level` on
  Gemini 3 and later (the Interactions transport too), `OpenRouterClient` sends
  `reasoning.effort`, `DeepSeekClient` sends `reasoning_effort`, and `GroqClient` sends
  `reasoning_effort` for GPT-OSS models. A model with no effort control — and Ollama, Bedrock
  and Azure AI Inference — ignores the value and logs at debug level; an OpenAI-compatible
  endpoint or an unrecognized model receives it only when the client was built with an
  effort. `None` keeps each client's own configuration.
