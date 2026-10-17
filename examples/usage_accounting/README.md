# Usage Accounting

Token usage that a provider reports with `null` or missing counts, through the real stack.
Every scenario drives `LlmAgentBuilder`, `Runner`, `InMemorySessionService` and a real
provider client, and asserts on the session's usage, the tool's run count and the run
budget's counters, never on model prose.

The scripted scenarios point the client at a local upstream on `127.0.0.1` through its base
URL. The upstream answers the first request with a `get_weather` call and the second with a
text answer, in the provider's own wire format, unary or streamed, and each usage object has
`null` or missing counts plus fields the typed client does not model. `LlmAgent` always
streams its model calls, so the `*_unary` scenarios wrap the client in a `NonStreaming`
adapter that sends `generate_content(request, false)`, the path direct callers such as
context compaction take. The `live_*` scenarios run when the provider's key is set.

## Scenarios

| Scenario | Upstream | Passes when |
|----------|----------|-------------|
| `openai_responses_unary`, `openai_responses_stream` | Scripted Responses API | `output_tokens`, `total_tokens` and the detail counts are `null`: the turn succeeds, `get_weather` runs once, each call's usage is the reported counts with the total summed, and `provider_usage` is the `usage` object sent |
| `anthropic_unary`, `anthropic_stream` | Scripted Messages API | `input_tokens`, streamed `message_delta.usage.output_tokens` and `server_tool_use.web_search_requests` are `null`: the same checks; a streamed message's `provider_usage` is the `message_start` usage with the `message_delta` fields applied over it, keeping `service_tier` and `inference_geo` |
| `gemini_unary`, `gemini_stream` | Scripted `generateContent` | `candidatesTokenCount`, `totalTokenCount` and `promptTokensDetails[].tokenCount` are `null` or missing: the same checks; `provider_usage` keeps `toolUsePromptTokenCount`, `trafficType` and `serviceTier` |
| `gemini_interactions_unary`, `gemini_interactions_stream` | Scripted Interactions API | `total_output_tokens`, `total_tokens` and `input_tokens_by_modality[].tokens` are `null`: the same checks |
| `deepseek_unary`, `deepseek_stream` | Scripted Chat Completions | `completion_tokens`, `total_tokens` and `prompt_cache_hit_tokens` are `null`: the same checks |
| `groq_unary`, `groq_stream` | Scripted Chat Completions | `completion_tokens` and `total_tokens` are `null`; the streamed finish chunk carries the usage in `usage` and `x_groq.usage`, as Groq's API does: the same checks, including on the tool-call turn |
| `ollama_unary` | Scripted `/api/chat` | The first final line leaves out `prompt_eval_count`, as Ollama does for a cached prompt: `eval_count` is still reported and `provider_usage` is the line's metric fields |
| `budget_groq_stream` | Scripted Chat Completions | A run budget with a token cap counts two model calls, one tool call and every reported token; a token cap fails closed on a call with no usage |
| `budget_ollama_unary` | Scripted `/api/chat` | The same for Ollama lines with and without `prompt_eval_count` |
| `live_openai_responses` | OpenAI | `get_weather` runs, and every model call's `provider_usage` is the Responses `usage` object, with `input_tokens_details` |
| `live_anthropic` | Anthropic | `get_weather` runs, and `provider_usage` keeps fields `adk_anthropic::Usage` does not model, such as `service_tier` |
| `live_gemini` | Gemini | `get_weather` runs, and `provider_usage` keeps fields `adk_gemini::UsageMetadata` does not model, such as `serviceTier` |

## Running

```bash
cp examples/usage_accounting/.env.example examples/usage_accounting/.env   # optional, for live scenarios
cargo run --manifest-path examples/usage_accounting/Cargo.toml
cargo run --manifest-path examples/usage_accounting/Cargo.toml -- --scenario groq_stream,budget_ollama_unary
```

Each scenario prints `PASS`, `FAIL`, or `SKIP` with its evidence; the process exits non-zero on
any `FAIL`. Set `ADK_ENV_FILE` to read keys from another dotenv file; its values override keys
already exported in the shell. `OPENAI_MODEL`, `ANTHROPIC_MODEL` and `GEMINI_MODEL` override
the live models (defaults `gpt-5.6-terra`, `claude-sonnet-4-6`, `gemini-3.7-flash`).
