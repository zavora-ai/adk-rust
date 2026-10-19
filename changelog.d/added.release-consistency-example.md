- **`release_consistency` example** (`examples/release_consistency`): drives the Gemini,
  Ollama, OpenRouter, Groq, DeepSeek, Azure AI, OpenAI Responses, and Bedrock clients through
  `LlmAgent` and `Runner` against the live Gemini API and a scripted local upstream, plus the
  compaction summarizers, `Evaluator`, and the server router, and checks generation config
  fields, stalled-request timeouts, unknown finish reasons, Responses error codes, compaction
  stream errors, custom eval criteria, and the `allowed_hosts` setters.
