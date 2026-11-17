- **Claude refusals carry their category** (`adk-anthropic`, `adk-model`): `Message::stop_details`
  and `MessageDelta::stop_details` keep the `stop_details` of a `refusal` stop, streamed and unary,
  instead of dropping it. `AnthropicClient` ends a refused turn with `error_code` `"refusal"`, an
  `error_message` naming the category and explanation, and `provider_metadata.stop_details`, so
  `LlmAgent` reports a failed turn instead of returning the partial text as an answer.
