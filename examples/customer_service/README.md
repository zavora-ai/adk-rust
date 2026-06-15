# Customer Service Agent — multimodal realtime support

A next-generation customer-support agent: it **sees** what you show the camera,
**hears** your voice and reads your tone, and takes **real actions** to resolve
issues — running on **either** OpenAI (`gpt-realtime`) **or** Gemini
(`gemini-3.1-flash-live-preview`) via a server-side bridge built on
`IntegratedRealtimeRunner`.

A redesigned take on Google's "Customer Service Agent" Live API demo — same
idea (multimodal + affective + tools), but backend-agnostic, in Rust, with a
themed UI.

## What it shows

| Capability | How |
|-----------|-----|
| **Multimodal** | The browser streams mic PCM **and** camera JPEG frames; the agent sees what you show it (`send_video_frame`). |
| **Affective dialogue** | Empathetic, tone-aware persona on both backends. Set `CS_AFFECTIVE=1` to additionally enable **Gemini's native affective-dialogue** (`enableAffectiveDialog`) — this switches Gemini to a native-audio model that adapts its tone to your emotion. |
| **Real actions (tools)** | `process_refund` (order id + reason → refund id) and `connect_to_human` (handoff) run **server-side**; results are spoken back. |
| **Either backend** | OpenAI or Gemini, chosen per session; audio rates negotiated to the browser (OpenAI 24 kHz in/out; Gemini 16 kHz in / 24 kHz out). |
| **Themed UI** | System / Light / Dark, persisted; responsive 3-column layout (highlights · conversation · camera). |

## Architecture

```text
┌──────────────────────── Browser ────────────────────────┐
│  mic PCM16 + camera JPEG (base64 over WS)                │
│  ◀── agent PCM16 + transcripts + tool events             │
└─────────────┬───────────────────────────────▲───────────┘
              │ /ws?provider=openai|gemini      │
┌─────────────▼───────────────────────────────┴───────────┐
│  Axum server (localhost:3066)                            │
│   IntegratedRealtimeRunner                               │
│   ├─ OpenAI gpt-realtime  OR  Gemini Live (native A/V)   │
│   ├─ send_audio / send_video_frame                       │
│   ├─ process_refund tool   (server-side)                 │
│   └─ connect_to_human tool (server-side)                 │
└──────────────────────────────────────────────────────────┘
```

The agent connection and the API key live entirely on the server; the browser
is just an audio/video device.

### Video, per backend

Camera frames are sent via the realtime crate's `send_video_frame`:

- **Gemini Live** ingests continuous video frames natively (sent ~1.4 fps here).
- **OpenAI Realtime** takes images as in-context items, so the UI sends periodic
  snapshots (~every 2.5 s) rather than a continuous stream.

## Prerequisites

- Rust 1.94+
- `OPENAI_API_KEY` (OpenAI) and/or `GEMINI_API_KEY` / `GOOGLE_API_KEY` (Gemini)
- A browser with WebSocket + Web Audio + mic/camera access

## Run

```bash
cargo run --manifest-path examples/customer_service/Cargo.toml
# → open http://localhost:3066

# Headless smoke test (no browser): asks for a refund by text, checks the path.
cargo run --manifest-path examples/customer_service/Cargo.toml -- probe openai
cargo run --manifest-path examples/customer_service/Cargo.toml -- probe gemini
```

Then **Connect**, press **Start mic** and/or **Start camera**, and try:

- *"Can I get a refund for order A-10293?"* → `process_refund`
- *"I want to return this — can you see it?"* (hold an item to the camera)
- *"I'm really frustrated with this!"* → empathetic tone
- *"I need to speak to a real person."* → `connect_to_human`

## Environment variables

| Variable | Required | Description |
|----------|----------|-------------|
| `OPENAI_API_KEY` | For OpenAI | OpenAI API key |
| `GEMINI_API_KEY` / `GOOGLE_API_KEY` | For Gemini | Google AI Studio key |
| `OPENAI_REALTIME_MODEL` | No | Default `gpt-realtime` |
| `GEMINI_REALTIME_MODEL` | No | Default `models/gemini-3.1-flash-live-preview` (or a native-audio model when `CS_AFFECTIVE=1`) |
| `CS_AFFECTIVE` | No | `1` to enable Gemini native affective dialogue (uses a native-audio model; trades some tool-calling reliability) |
| `PORT` | No | Server port (default `3066`) |

## Feature flags

```toml
adk-realtime = { version = "1.1.0", features = ["openai", "gemini", "integration"] }
```

> Multimodal video input is provided by `RealtimeSession::send_video_frame`
> (Gemini media chunks; OpenAI `input_image` items), exposed on
> `IntegratedRealtimeRunner`.
