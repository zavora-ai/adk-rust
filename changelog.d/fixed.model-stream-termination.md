- **Streams that end early fail the turn** (`adk-model`): the Chat Completions path
  (`OpenAIClient`, `OpenAICompatible` and every preset, `AzureOpenAIClient`, OpenCode Chat),
  `GroqClient`, `OllamaModel`, and `OpenRouterClient` (Chat Completions and Responses)
  returned the text received so far as a successful answer when the stream ended before its
  terminal event, and dropped a tool call that was still arriving (`OllamaModel` ran it).
  They now fail with a retryable `Unavailable` error (`model.<provider>.incomplete_stream`,
  `model.openrouter.chat_stream_incomplete`, `model.openrouter.responses_stream_incomplete`),
  so `LlmAgent` persists no truncated answer and runs no truncated tool call.
- **Errors inside a stream fail the turn** (`adk-model`): a chunk with a top-level `error`
  object or a `finish_reason` of `error` on the Chat Completions path and Groq, an Ollama
  `{"error": ...}` line, an OpenRouter `response.failed` event, and a non-streaming
  OpenRouter Responses body with status `failed` are errors with the provider's category and
  code, quoting at most 512 bytes of its message; they were skipped or read as a normal
  finish. OpenRouter `response.incomplete` finishes with `MaxTokens` or `Safety`, or fails
  for any other reason. A Groq chunk that does not decode fails with `model.groq.invalid_chunk`
  instead of being dropped with its tool call. Unknown finish reasons on the Chat Completions
  path and in Groq streams map to `FinishReason::Other` instead of `Stop`.
- **DeepSeek and Azure AI stream failures keep the provider's message and category**
  (`adk-model`): an error frame inside a `DeepSeekClient` or `AzureAIClient` stream failed
  with `model.legacy` (`invalid DeepSeek event data`, `incomplete Azure AI event stream`).
  It now fails with `model.deepseek.stream_error` or `model.azure_ai.stream_error`, quoting
  the provider's message and recording its code. A stream that ends before a finish reason
  or whose connection fails fails with `model.<provider>.incomplete_stream` (`Unavailable`,
  or `Timeout` after the idle timeout), and an undecodable chunk with
  `model.<provider>.invalid_chunk`.
- **SSE framing** (`adk-model`): `data:` fields without a space after the colon are read by
  the Chat Completions and Groq parsers, and Groq, Ollama, and OpenRouter decode a UTF-8
  character split across network chunks instead of replacing or dropping it.
  `OpenRouterSseDecoder::push_bytes` decodes raw bytes. `OllamaModel` calls `/api/chat`
  directly instead of through `ollama-rs`, which skipped error lines.
- **OpenRouter Responses streams persist the answer once** (`adk-model`): the terminal
  response carries the complete content, including tool calls, and replaces the streamed
  deltas, so `LlmAgent` no longer stores every answer twice. An unfinished or unnamed tool
  call fails the turn, and arguments that are not a JSON object become
  `adk_core::invalid_tool_arguments`, in Chat Completions and Responses; both were dropped.
