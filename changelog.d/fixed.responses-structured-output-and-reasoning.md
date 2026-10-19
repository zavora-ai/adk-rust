- **OpenAI Responses sends `output_schema` and replays reasoning correctly** (`adk-model`):
  `OpenAIResponsesClient` ignored `response_schema` (an agent's `output_schema`); it is now sent
  as a strict `text.format` `json_schema`, as Chat Completions sends `response_format`, which
  also constrains the `OpenAIClient` tool turns that GPT-5.6 and later models route through the
  Responses API. `Part::Thinking` is no longer replayed as an assistant message, which the model
  read as an answer it had given; reasoning goes back only as its reasoning item, ahead of the
  turn's other output.
