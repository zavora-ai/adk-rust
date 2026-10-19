# release_live_checks

Live and scripted checks for provider and runtime fixes from the 3.0 README audit. Each scenario
drives the real client or `LlmAgent` + `Runner` stack and prints `PASS`, `FAIL`, or `SKIP`; the
process exits non-zero on any failure.

| Item | Scenario | Mode | Checks |
|------|----------|------|--------|
| 1 | `responses_background_pending` | scripted | A `background: true` submission answered `queued` yields its `response_id` and status, no error |
| 1 | `responses_foreground_strict` | scripted | A foreground request answered `in_progress` still fails with `invalid_terminal` |
| 1 | `responses_background_live` | live | `gpt-5.6-terra`: submit in background, poll to `completed`, read the answer |
| 2 | `openai_webrtc_round_trip` | live | OpenAI Realtime over WebRTC: connect, ask by text, receive audio and a transcript (`--features webrtc`) |
| 3 | `gemini_live_connect` | live | Gemini Live connects and answers without the application installing a rustls crypto provider |
| 4 | `groq_presets_listed`, `groq_presets_answer` | live | Every `GroqConfig` preset names a model Groq lists, and answers a turn |
| 5 | `bench_reports_measured_metrics` | live | `BenchRunner` reports the model it called and measured memory, and no unmeasured metric |
| 6 | `graph_agent_turn_past_fifty_steps` | scripted | A 70-step `GraphAgent` turn under `Runner` finishes when the graph's limit allows it |
| 7 | `ollama_token_cap` | live | A local Ollama model capped by `max_output_tokens` stops there and finishes with `MaxTokens` |

## Running

```bash
cp .env.example .env    # add the keys you have
cargo run --manifest-path examples/release_live_checks/Cargo.toml
cargo run --manifest-path examples/release_live_checks/Cargo.toml -- 1 6        # some items
cargo run --manifest-path examples/release_live_checks/Cargo.toml --features webrtc   # needs cmake
```

Keys are read from `ADK_ENV_FILE`, else the repository's `.env`, and override the shell's
environment. Scenario 7 needs a running Ollama server with the model pulled
(`ollama pull smollm2:135m`).
