- **Responses background submissions return the pending response** (`adk-model`): a
  non-streaming `OpenAIResponsesClient` request with `background: true`, and every deep
  research request, failed with `model.openai_responses.invalid_terminal` because the API
  answers it `queued` or `in_progress`. The submission now yields an `LlmResponse` with
  `response_id` and `status` in `provider_metadata["openai"]`, no content, and
  `turn_complete: false`, ready for `poll_response`. Foreground and streamed requests still
  require a terminal status.
