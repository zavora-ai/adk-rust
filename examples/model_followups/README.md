# Model follow-ups

Streamed Ollama usage with a missing count, DeepSeek and Azure AI stream error frames, and
Chat Completions refusals, through the stack a user runs.

Each scenario drives `LlmAgentBuilder`, `Runner`, an in-memory session service, and a real
`adk-model` client. The scripted scenarios point the client at a local upstream on
`127.0.0.1` that sends every write as its own HTTP chunk.

## Scenarios

| Scenario | Client | Upstream behaviour | Required outcome |
|----------|--------|--------------------|------------------|
| `ollama_stream_usage_sse`, `ollama_stream_usage_none` | `OllamaModel` | A streamed answer whose final line leaves out `prompt_eval_count` and `prompt_eval_duration`, as Ollama does for a cached prompt | The final event reports usage `(0, 6, 6)`, and `provider_usage` is the line's metric fields |
| `ollama_stream_budget` | `OllamaModel` | The same stream, under a run budget with a token cap | The budget counts the call's 6 tokens |
| `deepseek_stream_error_sse`, `deepseek_stream_error_none` | `DeepSeekClient` | Text, then `{"error": {"message", "type": "server_error", "code": "overloaded"}}`, then `[DONE]` | Run fails with `model.deepseek.stream_error`, `Unavailable`, the provider's message, and `provider_error_code`; nothing persisted |
| `deepseek_stream_answer` | `DeepSeekClient` | A complete answer | Run succeeds with the answer |
| `azure_ai_stream_error_sse`, `azure_ai_stream_error_none` | `AzureAIClient` | Text, then `{"error": {"code": "ServiceUnavailable", "message"}}`, then `[DONE]` | Run fails with `model.azure_ai.stream_error`, `Unavailable`, the provider's message, and `provider_error_code`; nothing persisted |
| `azure_ai_stream_answer` | `AzureAIClient` | A complete answer | Run succeeds with the answer |
| `chat_refusal_stream_sse`, `chat_refusal_stream_none` | `OpenAICompatible` | `delta.refusal` fragments with `content: null`, then `finish_reason: "stop"` | Run succeeds; the final event's text is the refusal, with `FinishReason::Safety` and no `error_code` |
| `chat_refusal_unary` | `OpenAICompatible`, unary | `message.refusal` with `content: null` | The same |
| `live_openai_chat_refusal` | `OpenAIClient` | A request OpenAI declines, under a strict `response_format` schema | The same |

The `_sse` and `_none` scenarios run in `StreamingMode::SSE` and `StreamingMode::None`.
`chat_refusal_unary` wraps the client so `LlmAgent` calls `generate_content(request, false)`.

`live_openai_chat_refusal` runs when `OPENAI_API_KEY` is set and uses `gpt-4.1-mini`
(`OPENAI_REFUSAL_MODEL`): it reports a safety refusal in the `refusal` field, while the
`gpt-5.6` models answer the same prompt inside `content`.

## Run

```bash
cargo run --manifest-path examples/model_followups/Cargo.toml
cargo run --manifest-path examples/model_followups/Cargo.toml -- --scenario chat_refusal_stream_sse,deepseek_stream_error_sse
```

Keys load from the repository root `.env` (or `ADK_ENV_FILE`). The example prints `PASS`,
`FAIL`, or `SKIP` per scenario and exits non-zero when any scenario fails.
