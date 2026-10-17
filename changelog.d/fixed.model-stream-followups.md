- **Streamed Ollama usage reports the counts that are present** (`adk-model`): a streamed
  final line without `prompt_eval_count` (Ollama leaves out zero metrics, such as a fully
  cached prompt) reported no usage, so run budgets counted no tokens for the call. It now
  reports `eval_count`, and `provider_usage` holds the line's metric fields, as the
  non-streaming path does.
- **DeepSeek and Azure AI stream errors keep the provider's message and category**
  (`adk-model`): an error frame inside a `DeepSeekClient` or `AzureAIClient` stream failed
  with `model.legacy` (`invalid DeepSeek event data`, `incomplete Azure AI event stream`).
  It now fails with `model.deepseek.stream_error` or `model.azure_ai.stream_error`, quoting
  the provider's message and keeping its code, with the category of the other clients. A
  stream that ends before a finish reason, a connection that fails mid-stream, and an
  undecodable chunk fail with `model.<provider>.incomplete_stream` (`Unavailable`,
  retryable) and `model.<provider>.invalid_chunk`.
- **Chat Completions refusals reach the agent** (`adk-model`): a `refusal` with
  `content: null`, streamed or not, became an empty turn finishing with `Stop` on
  `OpenAIClient`, `OpenAICompatible` and its presets, `AzureOpenAIClient`, and OpenCode
  Chat. The refusal text is now the response's content and the turn finishes with
  `FinishReason::Safety`.
