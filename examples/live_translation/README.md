# Live Translation — real-time speech-to-speech in the browser

A web app that translates your speech into a target language in real time. Speak
English, hear (and read) Spanish a couple of seconds later. Same **server-side
bridge** architecture as the "Mindfulness with Mia" voice example — the browser
is a thin audio device, the Rust server owns the provider connection — but
pointed at the providers' **dedicated translation models**:

| Provider | Model | Notes |
|----------|-------|-------|
| **OpenAI** | `gpt-realtime-translate` | 70+ source languages → 13 target languages. Interpreter endpoint, 24 kHz PCM16 in/out. |
| **Gemini** | `gemini-3.5-live-translate-preview` | 70+ languages, auto-detects source. Live API, 16 kHz in / 24 kHz out. |

## What this shows

- **Real-time speech translation** — stream mic audio up, get translated audio +
  transcripts streamed back, a few seconds behind the speaker.
- **Two providers, one UI** — pick the model and target language; the server
  negotiates the per-provider audio rates to the browser before audio flows.
- **A faithful, minimal bridge** to each provider's translation protocol (these
  are *not* the standard realtime/voice-agent APIs — see below).

## Why a direct bridge (not `IntegratedRealtimeRunner`)

Unlike the Mia example, these are **purpose-built translation endpoints** with
their own protocols, so they don't fit the conversational
`IntegratedRealtimeRunner`:

- **OpenAI** uses a separate endpoint, `wss://api.openai.com/v1/realtime/translations`.
  It's a continuous *interpreter*: you `session.input_audio_buffer.append` audio,
  set the target via `session.update` → `session.audio.output.language`, and
  receive `session.output_audio.delta` / `session.output_transcript.delta` /
  `session.input_transcript.delta`. There is **no** `response.create`.
- **Gemini** uses the standard Live API endpoint with a
  `generationConfig.translationConfig` (`targetLanguageCode`, `echoTargetLanguage`)
  plus `inputAudioTranscription` / `outputAudioTranscription`.

So `src/translate.rs` talks to each endpoint directly over a WebSocket and emits a
common `XlatEvent` stream (translated audio + source/target transcripts) that
`src/server.rs` bridges to the browser.

> **Source transcript note:** `gpt-realtime-translate` streams the *translation*
> transcript and audio but does not emit a source-language transcript, so the
> "You said" pane stays empty on OpenAI. Gemini emits both.

## Architecture

```text
┌───────────────────────── Browser (Web UI) ─────────────────────────┐
│  mic ──PCM16 (base64 over WS)──▶        ◀── translated PCM16 + text  │
│  Web Audio capture @ in-rate            Web Audio playback @ 24 kHz  │
└───────────┬─────────────────────────────────────────────▲──────────┘
            │ WebSocket /ws?provider=…&target=…            │
┌───────────▼─────────────────────────────────────────────┴──────────┐
│  Axum server (localhost:3055)                                       │
│   src/translate.rs ── WS ──▶  OpenAI /realtime/translations   OR    │
│                                Gemini BidiGenerateContent (translate)│
└─────────────────────────────────────────────────────────────────────┘
```

The provider API key never reaches the browser — the translation connection
lives entirely server-side.

## Prerequisites

- Rust 1.95+
- `OPENAI_API_KEY` (for OpenAI) and/or `GEMINI_API_KEY` / `GOOGLE_API_KEY` (for Gemini)
- A modern browser with WebSocket + Web Audio + microphone access

## Run

```bash
cargo run --manifest-path examples/live_translation/Cargo.toml
# → open http://localhost:3055, pick a target language, press “Start translating”, and speak
```

### Headless probe (no browser/mic)

```bash
# Connectivity only — validates endpoint, auth, and model id:
cargo run --manifest-path examples/live_translation/Cargo.toml -- probe openai
cargo run --manifest-path examples/live_translation/Cargo.toml -- probe gemini

# True end-to-end — feed a raw PCM16 file (matching the provider's input rate)
# and see the translation. Synthesize one with macOS `say` + ffmpeg:
say -o /tmp/s.aiff "Hello, I would like to order a large coffee and a croissant."
ffmpeg -y -i /tmp/s.aiff -ar 24000 -ac 1 -f s16le /tmp/s24k.pcm   # OpenAI: 24 kHz
ffmpeg -y -i /tmp/s.aiff -ar 16000 -ac 1 -f s16le /tmp/s16k.pcm   # Gemini: 16 kHz
cargo run --manifest-path examples/live_translation/Cargo.toml -- probe openai es /tmp/s24k.pcm
cargo run --manifest-path examples/live_translation/Cargo.toml -- probe gemini es /tmp/s16k.pcm
# → probe: complete … translation="Hola, … un café grande y un cruasán. …" audio_bytes=…
```

## Environment variables

| Variable | Required | Description |
|----------|----------|-------------|
| `OPENAI_API_KEY` | For OpenAI | OpenAI API key |
| `GEMINI_API_KEY` / `GOOGLE_API_KEY` | For Gemini | Google AI Studio key |
| `OPENAI_TRANSLATE_MODEL` | No | Override (default `gpt-realtime-translate`) |
| `GEMINI_TRANSLATE_MODEL` | No | Override (default `gemini-3.5-live-translate-preview`) |
| `PORT` | No | Server port (default `3055`) |
| `RUST_LOG` | No | Log level (default `info`) |

## Target languages

The UI offers a common set (Spanish, French, German, Italian, Portuguese,
Japanese, Korean, Mandarin, Hindi, Arabic, Russian, Indonesian, English) as
BCP-47 codes. OpenAI supports 13 target languages; Gemini supports 70+, so you
can pass other `targetLanguageCode`s to the Gemini path via the `target` query
param.

> These are **preview** models; ids and capabilities may change. Override with
> the env vars above if needed.
