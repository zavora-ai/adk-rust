- **A `null` or missing token count no longer fails the turn** (`adk-model`, `adk-anthropic`,
  `adk-gemini`): OpenAI Responses (`input_tokens`, `output_tokens`, `total_tokens`, the
  detail counts), Anthropic (`input_tokens`, streamed `message_delta.usage.output_tokens`,
  `server_tool_use.*`), Gemini (`promptTokensDetails[].tokenCount` and the other detail
  lists), Gemini Interactions `Usage`, DeepSeek and Groq, unary and streaming, read such a
  count as unknown: zero for the totals, `None` for the breakdowns. The tool call in the same
  response now runs; a streamed Groq or Interactions tool call was dropped before. A missing
  total is the sum of the counts that were reported. `adk_core::nullable_count` is the shared
  serde helper for counts that may be `null`.
- **Groq streamed usage is reported on tool-call turns** (`adk-model`): the finish chunk's
  usage, from `usage` or `x_groq.usage`, now reaches the tool-call response; it was dropped,
  so run budgets and spend tracking saw no usage for those calls.
- **Ollama usage reports the counts that are present** (`adk-model`): a non-streaming final
  line without `prompt_eval_count` (Ollama leaves out zero metrics, such as a fully cached
  prompt) still reports `eval_count`; usage was dropped entirely. `OllamaModel` sends
  non-streaming requests to `/api/chat` directly so it can read the line as received.
