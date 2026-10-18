- **Generation config fields reach the provider or fail by name** (`adk-model`):
  `stop_sequences`, `seed`, `frequency_penalty`, `presence_penalty`, and `top_logprobs` were
  dropped by every non-Gemini client. Chat Completions clients now send all five (log
  probabilities return in `provider_metadata["logprobs"]`), Anthropic and Bedrock send stop
  sequences, Groq and Azure AI Inference send stop sequences, seed, and penalties, DeepSeek
  sends stop sequences and penalties, and the Responses API sends `top_logprobs`. A field a
  client cannot honour now fails the request before it is sent with
  `model.unsupported_config` naming the field, where it was silently ignored before; remove
  the field from the `GenerateContentConfig` for that provider.
