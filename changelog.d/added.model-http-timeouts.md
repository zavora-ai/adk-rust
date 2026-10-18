- **`HttpTimeouts` for model clients** (`adk-model`): the OpenAI-compatible, `OpenAIClient`,
  Azure OpenAI, OpenAI Responses, Azure AI Inference, Groq, DeepSeek, OpenRouter, Ollama, and
  Bedrock clients bound connecting (30 seconds) and every wait for data (10 minutes) by
  default, so a stalled stream fails with `ErrorCategory::Timeout` instead of hanging the run;
  `with_timeouts` sets both bounds.
