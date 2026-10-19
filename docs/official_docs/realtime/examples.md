# Realtime Examples

Four runnable examples in the [`examples/`](https://github.com/zavora-ai/adk-rust/tree/main/examples)
directory. `realtime_tools` is a headless OpenAI console probe; the three web examples use
the [server-side bridge](building-web-apps.md) and run on OpenAI or Gemini. Set the
relevant key first:

```bash
export OPENAI_API_KEY=sk-…        # for OpenAI sessions
export GEMINI_API_KEY=…           # for Gemini sessions (or GOOGLE_API_KEY)
```

---

## `realtime_tools`

A console program (no browser) on OpenAI Realtime that exercises [tool dispatch](tools.md)
on `IntegratedRealtimeRunner` with weather, calculator, and time tools, with session
persistence, memory storage, and transcript aggregation wired in. Requires
`OPENAI_API_KEY`.

```bash
cargo run --manifest-path examples/realtime_tools/Cargo.toml
```

---

## `realtime_voice` — *Mindfulness with Mia*

A web voice app: the browser captures microphone PCM16 and plays the agent gaplessly with
barge-in; the Rust server owns the session. Its [memory](memory.md) is a file-backed
`GraphMemoryService`: the profile card is injected at session start, every turn is logged to
the graph, and the agent records durable facts through the bridged `remember` and `relate`
tools. A "User Memory Insights" panel reads and writes the same graph over `/api/memory`. It
also has a server-side `get_weather` tool.

| Light | Dark |
|-------|------|
| ![Mindfulness with Mia — light theme](images/mia-light.png) | ![Mindfulness with Mia — dark theme](images/mia-dark.png) |

```bash
cargo run --manifest-path examples/realtime_voice/Cargo.toml
# → http://localhost:3033
```

Read it for: Web Audio capture and playback, knowledge-graph memory, agent-curated memory.

---

## `customer_service` — *Aria*

A customer-support agent on OpenAI or Gemini with a themed (system, light, dark) UI:

- **[Multimodal](multimodal.md)** — streams microphone PCM and camera JPEG frames
  (`send_video_frame`), so the agent sees what you show it.
- **[Tools](tools.md)** — `process_refund` and `connect_to_human` run server-side;
  results are spoken back.
- **[Affective dialogue](affective-dialogue.md)** — `CS_AFFECTIVE=1` switches Gemini to the
  native-audio model and turns the flag on.

| Light | Dark |
|-------|------|
| ![Customer Service Agent — light theme](images/customer-service-light.png) | ![Customer Service Agent — dark theme](images/customer-service-dark.png) |

```bash
cargo run --manifest-path examples/customer_service/Cargo.toml
# → http://localhost:3066

# headless smoke test (asks for a refund by text and checks that process_refund runs):
cargo run --manifest-path examples/customer_service/Cargo.toml -- probe openai
cargo run --manifest-path examples/customer_service/Cargo.toml -- probe gemini

# enable Gemini native affective dialogue:
CS_AFFECTIVE=1 cargo run --manifest-path examples/customer_service/Cargo.toml
```

Read it for: video input, mixing multimodal + tools + affect in one agent.

---

## `live_translation`

A speech-to-speech translator in the browser. It uses the same bridge with the providers'
translation models — OpenAI `gpt-realtime-translate` and Gemini
`gemini-3.5-live-translate-preview` — which use their own protocols. Pick a provider and a
target language in the UI, speak, and hear and read the translation.

| Light | Dark |
|-------|------|
| ![Live Translation — light theme](images/translation-light.png) | ![Live Translation — dark theme](images/translation-dark.png) |

```bash
cargo run --manifest-path examples/live_translation/Cargo.toml
# → http://localhost:3055
# headless connectivity probe (validates endpoint, auth, and model):
cargo run --manifest-path examples/live_translation/Cargo.toml -- probe openai
cargo run --manifest-path examples/live_translation/Cargo.toml -- probe gemini
```

Read it for: using the dedicated translation models and a focused, minimal bridge.

---

## A suggested path

1. **`realtime_tools`** — the runner and a tool turn, without audio.
2. **`realtime_voice`** — audio and knowledge-graph memory.
3. **`customer_service`** — video, tools, and affective dialogue.
4. **`live_translation`** — dedicated translation models.

Each example's `README.md` lists its environment variables, including the model overrides
`OPENAI_REALTIME_MODEL` and `GEMINI_REALTIME_MODEL` of the web examples.

← Back to the [Realtime overview](index.md)
