- **Streamed Responses keep the whole answer** (`adk-model`): `OpenAIResponsesClient` streams
  dropped text and reasoning that arrived only in a done event or in the terminal response,
  dropped refusals, streamed or not, and lost tool calls when the terminal response left out the
  output its `response.output_item.done` events had carried. Each final text now reaches the
  turn once — a done event adds only what its deltas did not, and corrected text replaces the
  streamed fragments. Refusals become the response text, raw reasoning text
  (`response.reasoning_text.*`) becomes `Part::Thinking` like reasoning summaries, and requests
  with native tools stream instead of falling back to one non-streaming response.
- **Unknown Responses stream events are skipped** (`adk-model`): an event whose `type` the
  client does not know, such as a new native-tool event, ended the turn with
  `model.openai_responses.parse`; it is now skipped. A known event with missing fields still
  fails.
