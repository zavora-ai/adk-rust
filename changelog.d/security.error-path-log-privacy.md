- **Error paths keep request content out of logs and traces** (`adk-agent`, `adk-model`,
  `adk-gemini`, `adk-anthropic`, `adk-guardrail`, `adk-runner`, `adk-tool`): with
  `RunConfig::record_payloads` off, a provider error that echoed the prompt was logged in
  full by `LlmAgent`, the retry loop, and the Gemini and Anthropic clients; toolset errors,
  tool guardrail denial and revision reasons, and guardrail transform reasons were logged
  too, and `adk-gemini` logged every non-streaming response at TRACE. A failure is now logged
  by its diagnostics — component, category, code, upstream status, provider, provider code,
  and request ID — and `LlmAgent` adds the message, and a refused tool call's reason, only
  when `record_payloads` is set. Guardrail reasons still reach the caller and the model.
- **Provider error messages quote a bounded excerpt** (`adk-model`, `adk-agent`): an HTTP
  error body or a provider's error message is cut to 512 bytes in the `AdkError` message of
  the OpenAI Chat, Responses, and compatible clients, Azure AI, DeepSeek, Groq, Ollama,
  OpenRouter, Gemini, and Anthropic, and in the `model.provider_error` that `LlmAgent`
  returns for a provider-reported error. A response that cannot be decoded is described by
  its decoder error class and position instead of being quoted. Categories and codes are
  unchanged.
