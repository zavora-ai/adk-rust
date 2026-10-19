- **A stalled model request fails with `ErrorCategory::Timeout` on every client**
  (`adk-model`): `HttpTimeouts` documents a `Timeout` error, but the Groq, DeepSeek, and Azure
  AI clients reported a request without response headers as `Unavailable`, OpenRouter and the
  OpenAI Responses client reported every stall as `Unavailable`, and a complete
  (non-streamed) response whose body stalled was `Internal` on the Groq, DeepSeek, Azure AI,
  and OpenAI-compatible clients and `Unavailable` on Ollama. Each now fails with `Timeout`
  when a bound expires and keeps `Unavailable` for other transport failures. `BedrockClient`
  applies the default connect and read timeouts to the SDK client it builds, where it
  bounded only stream events, and reports an SDK timeout as `model.bedrock.timeout`
  (`Timeout`); a client passed to `from_client` keeps its own SDK timeouts.
