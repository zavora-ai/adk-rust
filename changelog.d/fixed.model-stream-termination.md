- **Streams that end early or carry an error fail the turn** (`adk-model`): the
  OpenAI-compatible Chat Completions path (`OpenAIClient` without tools, `AzureOpenAIClient`,
  OpenCode Chat, and every compatible preset), `GroqClient`, `OllamaModel`, and
  `OpenRouterClient` (Chat Completions and Responses) returned the text received so far as
  a successful answer when the stream ended before its terminal event, and dropped a tool
  call that was still arriving. They now fail with a retryable `Unavailable` error
  (`model.<provider>.incomplete_stream`, `model.openrouter.chat_stream_incomplete`,
  `model.openrouter.responses_stream_incomplete`), so `LlmAgent` persists no truncated
  answer and runs no truncated tool call.
- **Errors inside a stream fail the turn** (`adk-model`): a chunk with a top-level `error`
  object, an Ollama `{"error": ...}` line, a `finish_reason` of `error`, an OpenRouter
  `response.failed` event, and a non-streaming OpenRouter Responses body with status `failed`
  are errors with the provider's category and code, quoting at most 512 bytes of its message.
  OpenRouter `response.incomplete` finishes with `MaxTokens` or `Safety`, or fails for any
  other reason. A Groq chunk that does not decode, such as a finish chunk with `null` token
  counts, fails the turn instead of silently dropping its tool call. Unknown Chat Completions
  finish reasons map to `FinishReason::Other` instead of `Stop`.
- **SSE framing** (`adk-model`): `data:` fields without a space after the colon are read by
  the OpenAI-compatible and Groq parsers, and Groq, Ollama, and OpenRouter decode a UTF-8
  character split across network chunks instead of replacing or dropping it.
  `OpenRouterSseDecoder::push_bytes` decodes raw bytes. Ollama streams `/api/chat` directly
  instead of through `ollama-rs`, which skipped error lines.
- **OpenRouter Responses streams persist the answer once** (`adk-model`): the terminal
  response carries the complete content, including tool calls, and replaces the streamed
  deltas, so `LlmAgent` no longer stores every answer twice. An unfinished or unnamed tool
  call fails the turn, and arguments that are not a JSON object become
  `adk_core::invalid_tool_arguments`, in Chat Completions and Responses; both were dropped.
