# Release consistency

Checks the code findings of the 3.0 changelog audit against the real stack: `LlmAgent` and
`Runner` with the `adk-model` provider clients, `LlmEventSummarizer` and
`SummarisationCompaction`, `adk_eval::Evaluator`, and the `adk-server` router. Live scenarios
call the Gemini API; scripted scenarios point a real client at a local upstream that replays
provider responses or stalls, for behaviour a live API does not produce on demand.

## Scenarios

| Item | Scenario | Mode | Checks |
|------|----------|------|--------|
| 1 | `gemini-config-fields` | scripted | `GeminiModel` sends `stopSequences`, `seed`, `presencePenalty`, `frequencyPenalty`, `responseLogprobs`, and `logprobs`, and returns `logprobsResult` in `provider_metadata["logprobs"]` |
| 1 | `gemini-interactions-unsupported` | scripted | The Interactions transport fails on penalties and `top_logprobs` with `model.unsupported_config` before sending |
| 1 | `gemini-live-stop` | live | A stop sequence of `"5"` ends `gemini-3.5-flash-lite`'s count before `6` |
| 1 | `gemini-live-seed` | live | The same seed at temperature 2 gives the same answer twice |
| 1 | `gemini-live-penalty` | live | A presence penalty reaches Gemini, which refuses it for `gemini-3.5-flash-lite` with `InvalidInput` |
| 2 | `ollama-config-fields` | scripted | `OllamaModel` sends stop sequences, seed, and penalties in `options`, requests log probabilities, and returns them |
| 2 | `openrouter-responses-seed` | scripted | OpenRouter's Responses mode fails on `seed` with `model.unsupported_config` before sending |
| 2 | `ollama-live` | live | Against a local Ollama: the same seed gives the same answer twice, a stop sequence cuts it, and log probabilities return |
| 3 | `timeout-headers-*` | scripted | Groq, DeepSeek, Azure AI, OpenRouter (both APIs), OpenAI Responses, and Bedrock fail a request without response headers with `Timeout` |
| 3 | `timeout-stream-*` | scripted | OpenRouter (both APIs) and OpenAI Responses fail a stalled stream with `Timeout` |
| 3 | `timeout-body-*` | scripted | Groq, DeepSeek, Azure AI, Ollama, and OpenAI Responses fail a stalled complete response with `Timeout` |
| 4 | `finish-groq-complete`, `finish-deepseek-stream`, `finish-deepseek-complete` | scripted | An unknown finish reason reads as `FinishReason::Other` |
| 5 | `responses-error-type`, `responses-error-status` | scripted | The Responses client records `provider_error_code` from `type`, then `status`, when `code` is absent |
| 6 | `compaction-summarizer-stream-error`, `compaction-strategy-stream-error` | scripted | A summary whose stream stalls fails with `model.summary_incomplete`, keeping the `Timeout` category and the cause's code |
| 7 | `eval-custom-only` | local | An evaluator whose only criterion is custom is rejected with `ConfigError` |
| 8 | `hosts-security-config`, `hosts-server-config` | local | Both `with_allowed_hosts` setters replace the list |

## Running

```bash
cp .env.example .env    # or point ADK_ENV_FILE at an existing .env
cargo run               # every scenario
cargo run -- 1 3        # only these audit items
```

Keys load from `ADK_ENV_FILE`, else the repository root `.env`, overriding the shell
environment. Without `GEMINI_API_KEY` (or `GOOGLE_API_KEY`) the live Gemini scenarios are
skipped, and without `OLLAMA_MODEL` (a model pulled into the Ollama at `OLLAMA_HOST`, default
`http://127.0.0.1:11434`, such as `smollm2:135m`) the live Ollama scenario is.
The program prints one line per scenario, then a table, and exits non-zero when any scenario
fails.
