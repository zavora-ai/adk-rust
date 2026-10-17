# Stream termination

Model streams that end early or carry an error fail the run instead of persisting a
truncated answer.

Each scripted scenario runs the stack a user runs — `LlmAgentBuilder`, `Runner`, an
in-memory session service, and a real `adk-model` client — against a local upstream on
`127.0.0.1` that scripts one wire fault. The upstream sends every write as its own HTTP
chunk and ends each response cleanly, as a proxy does when the model stopped early.

## Scenarios

Every scripted scenario runs in `StreamingMode::SSE` and `StreamingMode::None`, for each
client: `OpenAIClient::compatible` (the shared Chat Completions path behind OpenAI Chat,
Azure OpenAI, OpenCode Chat, and every preset), `GroqClient`, `OllamaModel`, and
`OpenRouterClient` in Chat Completions and Responses mode.

| Scenario | Upstream behaviour | Required outcome |
|----------|--------------------|------------------|
| `complete-answer` | A complete answer | Run succeeds; the exact answer is persisted and in `output_key` state |
| `cut-mid-answer` | Ends mid-answer, before a finish reason or terminal event | Run fails; no answer persisted, no `output_key` state |
| `cut-mid-tool-call` | Ends while tool-call arguments are arriving (Ollama: after the call, before `done`) | Run fails; the tool does not run |
| `error-frame` | An error frame mid-answer (`{"error": ...}`, an Ollama error line, `response.failed`), then a normal end | Run fails; no answer persisted |
| `finish-reason-error` | `finish_reason: "error"` without an error object (OpenAI-compatible, Groq) | Run fails |
| `data-without-space` | Every field written `data:{...}` | Run succeeds with the exact answer |
| `split-multibyte` | `🙂` split across two network writes | Run succeeds with the exact answer |
| `tool-call` | A complete tool call, then the answer | The tool runs once; the exact answer is persisted |
| `tool-call-null-usage` | A Groq finish chunk with `null` token counts | The tool runs once; the exact answer is persisted |
| `direct .../status-failed` | A non-streaming OpenRouter Responses body with `status: "failed"` | `generate_content(_, false)` returns an error |

Live scenarios run when the key is set and show that normal streaming still succeeds in
both modes:

| Key | Scenarios |
|-----|-----------|
| `OPENAI_API_KEY` | OpenAI Chat Completions answer (`OPENAI_MODEL`, default `gpt-5.6-terra`) |
| `GROQ_API_KEY` | Groq tool call and answer (`GROQ_MODEL`, default `openai/gpt-oss-120b`) |
| `OPENROUTER_API_KEY` | OpenRouter Chat and Responses answers, and a Responses answer cut by `max_output_tokens` that must persist with `FinishReason::MaxTokens` (`OPENROUTER_MODEL`, default `openai/gpt-4.1-mini`) |

## Run

```bash
cargo run --manifest-path examples/stream_termination/Cargo.toml
```

For the live scenarios, add the keys listed in `.env.example` to `.env` in the repository
root or the current directory. Set `SKIP_LIVE=1` to run only the scripted scenarios. The
example prints `PASS` or `FAIL` per scenario and exits non-zero when any scenario fails.
