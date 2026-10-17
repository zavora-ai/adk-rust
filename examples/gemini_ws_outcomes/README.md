# Gemini and Responses WebSocket stream outcomes

Shows that Gemini responses and the OpenAI Responses WebSocket transport end with their real
outcome — an error, a safety stop, or a truncation — instead of an empty or partial success.

Scripted scenarios need no API keys: each one starts a local upstream on 127.0.0.1 and points
the real client at it. Live scenarios run when keys are set.

## Run

```bash
cargo run --manifest-path examples/gemini_ws_outcomes/Cargo.toml
```

The example reads keys from this crate's `.env` or the repository `.env` ahead of the shell,
so a stale exported key does not win. `ENV_FILE=/path/to/.env` names another file. Set
`SKIP_LIVE=1` to run only the scripted scenarios. The process exits non-zero when any
scenario fails.

## Scenarios

### Gemini (`GeminiModel` through `LlmAgentBuilder` and `Runner`)

Each fault runs in `StreamingMode::SSE` and `StreamingMode::None`, which both reach
`streamGenerateContent` because `LlmAgent` always streams from the model, and as a direct
`generate_content(request, false)` call against `generateContent`.

| Fault | Required outcome |
|-------|------------------|
| Complete answer | The run persists exactly the answer |
| `{"error": …}` object in the response | The turn fails; no answer or `output_key` state |
| Stream ends before any `finishReason` (body cut mid-JSON when not streaming) | The turn fails |
| `promptFeedback.blockReason` | A `Safety` turn whose `error_code` is the block reason; the agent run fails |
| `finishReason` `BLOCKLIST`, `PROHIBITED_CONTENT`, `SPII`, `IMAGE_SAFETY`, `MODEL_ARMOR` | `FinishReason::Safety` |

The Interactions transport (`use_interactions_api(true)`) must see `interaction.completed`
and fails on a `failed` status.

### Responses WebSocket (`WsTransport`)

| Scenario | Required outcome |
|----------|------------------|
| Complete answer | The answer arrives once across the deltas and the final chunk |
| Request frame | A `response.create` event without `stream` |
| `response.incomplete` at `max_output_tokens` | A final `MaxTokens` chunk, no hang |
| `response.incomplete` for another reason | An error, no hang |
| Close before a terminal event | An `Unavailable` error |
| `response.failed` | An error |
| `error` event with status 429 | A `RateLimited` error |
| Completed response that does not parse | An error |
| First response dropped mid-stream, then a second request | The second request gets only its own answer |
| Server closes the connection after a response | The next request reconnects and succeeds |
| First handshake answered with 503 | `connect` retries and succeeds |
| Handshake answered with 401 | One attempt, an `Unauthorized` error |
| Reconnect answered with 401 | One attempt, an `Unauthorized` error |

### Live

| Key | Scenarios |
|-----|-----------|
| `GEMINI_API_KEY` (or `GOOGLE_API_KEY`) | `gemini-3.5-flash-lite` answers through `generateContent` streaming in both modes and through the Interactions transport |
| `OPENAI_API_KEY` | `gpt-5.6-terra` answers over the WebSocket endpoint; a request dropped mid-stream is followed by a second request that gets its own answer |

`GEMINI_MODEL` and `OPENAI_MODEL` override the live models.
