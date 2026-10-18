- **OpenAI Responses sends `output_schema` and replays reasoning correctly** (`adk-model`): a
  `response_schema` (an agent's `output_schema`) is sent as a strict `text.format`
  `json_schema`, as Chat Completions sends `response_format`, so structured-output agents with
  tools on `OpenAIResponsesClient`, and on `OpenAIClient` for GPT-5.6 tool calls, are
  constrained. `Part::Thinking` is no longer replayed as an assistant message; reasoning goes
  back only as its reasoning item, ahead of the output it produced.
