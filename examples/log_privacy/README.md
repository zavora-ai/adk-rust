# Log privacy

Error paths keep user data and secrets out of logs and traces unless
`RunConfig::record_payloads` is on.

Each scenario runs the stack a user runs — `LlmAgentBuilder`, `Runner`, an in-memory
session service, and a real `adk-model` client — with a global tracing subscriber that
records every span field and event field at every level. The scripted scenarios point the
real clients at a local upstream on `127.0.0.1` whose responses echo a planted secret,
`acct-7731-secret`, followed by 4 KB of padding.

## Scenarios

With `record_payloads` off (the default), a scenario passes when the secret is in no log
line or span field, the failure's code, category, and upstream status are still logged, and
an error returned to the caller quotes at most a bounded excerpt of the upstream body.

| Scenario | What the upstream or the agent does |
|----------|-------------------------------------|
| `openai-compatible/http-400` | `OpenAIClient::compatible` gets a 400 whose message echoes the secret |
| `openai-compatible/retried-503` | A 503 echoing the secret, retried, then an answer; the retry is logged by category and status |
| `openai-compatible/stream-error` | An error frame echoing the secret mid-stream |
| `gemini/http-400` | `GeminiModel` gets a 400 whose message echoes the secret |
| `gemini/stream-error` | An error object echoing the secret inside a 200 stream |
| `gemini/answer-not-logged` | The answer holds the secret; it reaches no log |
| `gemini/direct-answer-not-logged` | A direct non-streaming `generate_content` call; the response reaches no log at any level |
| `anthropic/http-400` | `AnthropicClient` gets a 400 echoing the secret, reported inside the stream |
| `tool/credential-in-error` | A tool fails with a connection string holding the secret |
| `guardrail/path-denial` | `PathAllowList` denies a path holding the secret |
| `callback/error` | A before-tool callback fails with the secret in its message |
| `toolset/unreachable` | A toolset fails to resolve with a token in its URL |
| `record-payloads/*` | The same failures with `record_payloads` on: the message text is logged |
| `caller/run-outside-the-trace` | A run driven with `NoSubscriber` inside a caller span records nothing, while the caller's own events are kept |

A live scenario runs when `OPENAI_API_KEY` is set:

| Scenario | What it sends |
|----------|---------------|
| `live/openai-http-400` | An unrecognized request argument named after the secret, which OpenAI echoes in its 400 (`OPENAI_MODEL`, default `gpt-5.6-terra`) |

## Run

```bash
cargo run --manifest-path examples/log_privacy/Cargo.toml
```

The example loads the repository root's `.env`, overriding variables already set in the
shell; set `LOG_PRIVACY_ENV` to load another file. Set `SKIP_LIVE=1` to run only the
scripted scenarios. The example prints `PASS` or `FAIL` per scenario and exits non-zero
when any scenario fails.
