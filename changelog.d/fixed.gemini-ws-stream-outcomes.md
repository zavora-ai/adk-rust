- **Gemini errors, cut streams, and blocked prompts no longer read as an empty success**
  (`adk-gemini`, `adk-model`):
  - An `error` object in a successful `generateContent` body or `streamGenerateContent`
    event fails the call with `ClientError::ErrorInResponse`, categorized by the object's
    `code` or `status`, instead of decoding as an empty response.
  - A stream that ends before any chunk carries `finishReason` fails with
    `model.gemini.unfinished_stream` (`Unavailable`).
  - `promptFeedback.blockReason` ends the turn with `FinishReason::Safety` and `error_code`
    set to the block reason, so `LlmAgent` fails the run with `model.provider_error` instead
    of recording an empty turn.
  - The `BLOCKLIST`, `PROHIBITED_CONTENT`, `SPII`, `IMAGE_SAFETY`, and `MODEL_ARMOR` finish
    reasons map to `FinishReason::Safety` instead of `FinishReason::Other`.
  - The Interactions stream (`gemini-interactions`) fails with
    `model.gemini.interactions.unfinished_stream` when it ends before
    `interaction.completed`, and with `model.gemini.interactions.failed` or
    `model.gemini.interactions.budget_exceeded` on those statuses, which it reported as a
    successful `FinishReason::Other` turn.
- **The Responses WebSocket transport ends every response with its real outcome**
  (`adk-model`, `openai-ws`): `WsTransport::send_request` reads server events with the same
  projection and stream rules as `OpenAIResponsesClient`.
  - Requests are sent as `response.create` events, as WebSocket mode requires, without the
    HTTP-only `stream` and `background` fields; the endpoint answered the bare request body
    with an `error` event. The final chunk carries only what the deltas did not, instead of
    repeating the answer, and refusals, raw reasoning, and done-only text arrive as on the
    HTTP path.
  - `response.incomplete`, `response.cancelled`, and an event of unknown type whose
    `response.status` is terminal end the stream; the first two left it waiting forever.
  - A Close or end of the connection before a terminal event fails with
    `model.openai_responses.unfinished_stream` (`Unavailable`) instead of ending the stream
    without an error. `response.failed` fails with `model.openai_responses.response_failed`,
    an `error` event with `model.openai_responses.ws_error` categorized by its `status` and
    documented code, and a terminal response that does not parse with
    `model.openai_responses.parse`, instead of an `Ok` response carrying `error_code` or the
    raw JSON.
  - A stream dropped before its terminal event, or ended by an `error` event, discards the
    connection; the next request reconnects instead of reading the rest of the abandoned
    response. A connection the server closed while idle is replaced before the next request.
  - Handshakes, the first `connect` included, are retried under `RetryConfig` for network
    errors and HTTP 408, 429, 500, 502, 503, 504, and 529, honouring `Retry-After`; a 401 or
    other 4xx fails at once with the status's category.
    `WsTransport::connect_with_retry_config` sets the policy for `connect` and later
    reconnections.
