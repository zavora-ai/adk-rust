# Realtime Examples

Four runnable examples in [adk-playground](https://github.com/zavora-ai/adk-playground), and
eleven smaller ones [in this repository](#in-this-repository).
`realtime_tools` is a headless OpenAI console probe; the three web examples use
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

It serves on `http://localhost:3033`.

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

| Run with | Result |
|----------|--------|
| no arguments | The web app on `http://localhost:3066` |
| `probe openai` or `probe gemini` | A headless smoke test: asks for a refund by text and checks that `process_refund` runs |
| `CS_AFFECTIVE=1` in the environment | Gemini native affective dialogue |

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

| Run with | Result |
|----------|--------|
| no arguments | The web app on `http://localhost:3055` |
| `probe openai` or `probe gemini` | A headless connectivity probe that validates the endpoint, auth, and model |

Read it for: using the dedicated translation models and a focused, minimal bridge.

---

## In this repository

`adk-realtime/examples/` holds eleven programs, each run from a clone with the features it
requires:

| Example | Run with | Shows | Needs |
|---------|----------|-------|-------|
| `openai_session_update` | `cargo run -p adk-realtime --features openai --example openai_session_update` | A mid-session persona change through `session.update`, without dropping the connection | `OPENAI_API_KEY` |
| `gemini_context_mutation` | `cargo run -p adk-realtime --features gemini --example gemini_context_mutation` | The same change on Gemini Live, by session resumption | `GOOGLE_API_KEY` |
| `vertex_live_voice` | `cargo run -p adk-realtime --features vertex-live --example vertex_live_voice` | Gemini Live on Vertex AI with Application Default Credentials: a text prompt, text deltas and audio chunks | `GOOGLE_CLOUD_PROJECT`, ADC; `GOOGLE_CLOUD_REGION` (default `us-central1`) |
| `vertex_live_tools` | `cargo run -p adk-realtime --features vertex-live --example vertex_live_tools` | Tool calls during a Vertex AI Live session | `GOOGLE_CLOUD_PROJECT`, ADC |
| `debug_gemini` | `cargo run -p adk-realtime --features gemini --example debug_gemini` | A connectivity check for Gemini Live | `GEMINI_API_KEY` |
| `livekit_bridge` | `cargo run -p adk-realtime --features livekit,openai --example livekit_bridge` | A LiveKit room bridged to OpenAI Realtime | `LIVEKIT_URL`, `LIVEKIT_API_KEY`, `LIVEKIT_API_SECRET`, `OPENAI_API_KEY` |
| `livekit_gemini_bridge` | `cargo run -p adk-realtime --features livekit,gemini --example livekit_gemini_bridge` | The same bridge to Gemini Live, resampling the room's 24 kHz audio to 16 kHz | `LIVEKIT_URL`, `LIVEKIT_API_KEY`, `LIVEKIT_API_SECRET`, `GEMINI_API_KEY` |
| `debug_livekit_auth` | `cargo run -p adk-realtime --features livekit --example debug_livekit_auth` | A LiveKit access token and a room connection | `LIVEKIT_URL`, `LIVEKIT_API_KEY`, `LIVEKIT_API_SECRET` |
| `openai_webrtc` | `cargo run -p adk-realtime --features openai-webrtc --example openai_webrtc` | OpenAI Realtime over WebRTC instead of WebSocket (needs `cmake`) | `OPENAI_API_KEY` |
| `heygen_avatar` | `cargo run -p adk-realtime --features heygen-avatar --example heygen_avatar` | A HeyGen LiveAvatar configuration and how it attaches to a `RealtimeAgent`; it prints the configuration and opens no session | `HEYGEN_API_KEY` (optional) |
| `did_avatar` | `cargo run -p adk-realtime --features did-avatar --example did_avatar` | A D-ID Realtime Agents avatar configuration, whose own model and voice answer; it prints the configuration and opens no session | `DID_API_KEY`, `DID_AGENT_ID` (both optional) |

---

## A suggested path

1. **`realtime_tools`** — the runner and a tool turn, without audio.
2. **`realtime_voice`** — audio and knowledge-graph memory.
3. **`customer_service`** — video, tools, and affective dialogue.
4. **`live_translation`** — dedicated translation models.

Each example's `README.md` lists its environment variables, including the model overrides
`OPENAI_REALTIME_MODEL` and `GEMINI_REALTIME_MODEL` of the web examples.

← Back to the [Realtime overview](index.md)
