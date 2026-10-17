- **Gemini errors, cut streams, and blocked prompts no longer read as an empty success**
  (`adk-gemini`, `adk-model`):
  - An `error` object in a successful `generateContent` body or `streamGenerateContent`
    event fails the call, categorized by the object's `code` or `status`, instead of
    decoding as an empty response.
  - A stream that ends before any chunk carries `finishReason` fails with
    `model.gemini.unfinished_stream` (`Unavailable`).
  - `promptFeedback.blockReason` ends the turn with `FinishReason::Safety` and `error_code`
    set to the block reason, so `LlmAgent` fails the run instead of recording an empty turn.
  - The `BLOCKLIST`, `PROHIBITED_CONTENT`, `SPII`, `IMAGE_SAFETY`, and `MODEL_ARMOR` finish
    reasons map to `FinishReason::Safety` instead of `FinishReason::Other`.
  - The Interactions stream (`gemini-interactions`) fails when it ends before
    `interaction.completed`, and on a `failed` or `budget_exceeded` status, which it
    reported as a successful `FinishReason::Other` turn.
- **The Responses WebSocket transport ends every response with its real outcome**
  (`adk-model`, `openai-ws`):
  - A response stream dropped before its terminal event discards the connection; the next
    request reconnects instead of receiving the rest of the abandoned response.
  - `response.incomplete`, `response.cancelled`, and unknown event types carrying a
    terminal `response.status` end the stream; `response.incomplete` left it waiting forever.
  - A Close or end of the connection before a terminal event fails with
    `model.openai_responses.unfinished_stream`. `response.failed`, `error` events, and a
    completed response that does not parse are errors instead of an `Ok` response carrying
    `error_code` or the raw JSON.
  - Requests are sent as `response.create` events, as WebSocket mode requires; the live
    endpoint answered the previous frame with an `error` event. The final chunk carries
    only what the deltas did not, as on the HTTP path, instead of repeating the answer.
  - Handshakes, including the first `connect`, are retried for network errors, 408, 429,
    and 5xx under `RetryConfig`; a 401 or other 4xx fails at once with the status's
    category. `WsTransport::connect_with_retry_config` sets the policy. A connection the
    server closed while idle is replaced before the next request.
