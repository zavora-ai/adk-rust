- **Responses parse errors no longer quote the response** (`adk-model`): a Responses body or
  stream event that did not decode failed with `model.openai_responses.parse` and a message
  carrying the whole response body, generated text and tool arguments included. The message is
  now `OpenAI Responses API parse error`, and `details.metadata["openai_responses_parse"]`
  describes the failure by layer (`stream_setup`, `stream_event`, `response_body`), serde
  category, line, column and cause. It names the field only when it is a known Responses schema
  field, and the event type only when it is `error` or a `response.*` identifier.
