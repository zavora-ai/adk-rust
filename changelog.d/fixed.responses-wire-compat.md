- **Responses calls end with their real outcome** (`adk-model`): `OpenAIResponsesClient` applies
  the same terminal rules to streamed and non-streaming responses.
  - A `failed` response, a `response.failed` event, or an `error` event fails with
    `model.openai_responses.response_failed`. A non-streaming failed response was an empty
    successful turn, and a streamed one an `Ok` response carrying the provider's code and
    message, which `LlmAgent` turned into a non-retryable `model.provider_error`. A code from
    OpenAI's documented `response.error.code` values is recorded in `provider_error_code` and
    the message and sets the category: `rate_limit_exceeded` is `RateLimited` and
    `server_error` is `Unavailable`, both retryable, `vector_store_timeout` is `Timeout`, and
    the input codes are `InvalidInput`. Other codes and provider messages stay out of the error,
    which is then `Internal`.
  - A cancelled response fails with `model.openai_responses.cancelled` (`Cancelled`), and an
    `incomplete` one with `model.openai_responses.incomplete` unless it stopped at
    `max_output_tokens` (`FinishReason::MaxTokens`) or the content filter
    (`FinishReason::Safety`); both were successful turns.
  - A tool call that is unfinished, sits in a response that did not complete, or has an empty
    or duplicate call ID fails with `model.openai_responses.invalid_terminal` or
    `model.openai_responses.invalid_identity` instead of running.
  - A stream that ends before its terminal event fails with
    `model.openai_responses.unfinished_stream` (`Unavailable`, retryable) instead of ending
    without an error. A terminal response that lacks a tool call the stream announced fails with
    the same code as a non-retryable `Internal` error.
  - Lifecycle events and terminal responses decode only the fields the client reads, so an
    echoed request field holding a value the SDK types do not model, such as a new tool type, no
    longer fails the turn with `model.openai_responses.parse`. `response.done` and
    `response.canceled` are read as terminal events.
