- **A `null` or missing token count no longer fails the turn** (`adk-model`, `adk-anthropic`,
  `adk-gemini`): such a count failed the response on OpenAI Responses (`input_tokens`,
  `output_tokens`, `total_tokens`, the detail counts; `model.openai_responses.parse`),
  Anthropic (`input_tokens`, streamed `message_delta.usage.output_tokens`,
  `server_tool_use.web_search_requests`), Gemini (`promptTokensDetails[].tokenCount` and the
  other per-modality lists), Gemini Interactions `Usage`, DeepSeek and Groq, unary and
  streamed. A streamed Groq finish chunk or Interactions `interaction.completed` event that
  carried one was dropped with its tool call. The count now reads as zero, or `None` for an
  optional `UsageMetadata` breakdown such as cached or reasoning tokens, a missing total is
  the sum of the counts that were reported, and the tool call runs. `adk_core::nullable_count`
  is the shared serde helper for a count that may be `null`.
- **Groq streamed usage reaches tool-call turns** (`adk-model`): the finish chunk's usage, from
  `usage` or else `x_groq.usage`, is reported on the tool-call response, which carried none.
- **Ollama usage reports the counts that are present** (`adk-model`): a final `/api/chat`
  line without one of its metric fields, such as `prompt_eval_count` (Ollama leaves out zero
  metrics, as for a fully cached prompt), reported no usage at all, streamed or not. A missing
  count now reads as zero, `eval_count` is still reported, and `provider_usage` holds the
  metric fields the line carried.
