# Realtime Providers

ADK-Rust speaks two realtime backends behind the same `RealtimeModel` interface, so an
application can offer both and switch per session. This page covers the models, voices,
endpoints, and auth.

## OpenAI Realtime

```rust
use adk_realtime::model::BoxedModel;
use adk_realtime::openai::OpenAIRealtimeModel;
use std::sync::Arc;

fn openai_model() -> anyhow::Result<BoxedModel> {
    Ok(Arc::new(OpenAIRealtimeModel::new(std::env::var("OPENAI_API_KEY")?, "gpt-realtime-2.1")))
}
```

| Model | Use |
|-------|-----|
| `gpt-realtime-2.1` | Speech-to-speech conversations; the catalog default (`adk_model::catalog::OPENAI_REALTIME_DEFAULT`) |
| `gpt-realtime-translate` | Translation, on its own endpoint (`/v1/realtime/translations`); see the [`live_translation` example](examples.md#live_translation) |

- **Transport**: WebSocket (`openai` feature) or WebRTC (`openai-webrtc`, needs `cmake`).
  WebRTC mints an ephemeral key with `POST /v1/realtime/client_secrets` for a session built
  from the `RealtimeConfig`, exchanges SDP with `POST /v1/realtime/calls`, and carries audio
  as Opus over UDP from the interface that routes to OpenAI; events use the `oai-events`
  data channel.
- **Audio**: 24 kHz PCM16 in and out.
- **Voices**: `marin`, `alloy`, and the other OpenAI voices; set with
  `RealtimeConfig::with_voice("marin")`.
- **Auth**: `OPENAI_API_KEY`. The key stays on your server.

## Gemini Live

```rust
use adk_realtime::gemini::{GeminiLiveBackend, GeminiRealtimeModel};
use adk_realtime::model::BoxedModel;
use std::sync::Arc;

fn gemini_model() -> anyhow::Result<BoxedModel> {
    Ok(Arc::new(GeminiRealtimeModel::new(
        GeminiLiveBackend::studio(std::env::var("GEMINI_API_KEY")?),
        "models/gemini-3.1-flash-live-preview",
    )))
}
```

| Model (AI Studio) | Use |
|-------------------|-----|
| `models/gemini-3.1-flash-live-preview` | Conversations with tools and video frames; the catalog default (`GEMINI_LIVE_DEFAULT`) and the examples' default |
| `models/gemini-3.8-live` | Conversations; camera frames reach it through `realtimeInput.video` |
| `models/gemini-2.5-flash-native-audio-preview-12-2025` | Native audio, required for [affective dialogue](affective-dialogue.md); the catalog lists it as deprecated |
| `models/gemini-3.5-live-translate-preview` | Translation; see the [`live_translation` example](examples.md#live_translation) |

- **Transport**: WebSocket (`gemini` feature) to AI Studio, or Vertex AI Live
  (`vertex-live`, OAuth2 / ADC — see [Realtime Agents](../agents/realtime-agents.md#vertex-ai-live-google-cloud)).
  `GeminiLiveBackend::studio` connects to AI Studio's `v1alpha` endpoint.
- **Model names differ by endpoint**: `gemini-live-2.5-flash-native-audio` is the Vertex AI
  name; AI Studio answers it with an error.
- **Bare ids**: AI Studio's setup takes the resource name `models/{id}` and closes the socket
  with 1007 on a bare id, so `GeminiRealtimeModel::new` adds the prefix: `gemini-3.1-flash-live-preview`
  and `models/gemini-3.1-flash-live-preview` name the same model, and `model_id()` reports the
  prefixed name. Vertex AI names are sent as given.
- **Voice activity**: the server's `voiceActivity` messages arrive as `ServerEvent::SpeechStarted`
  (`ACTIVITY_START`) and `ServerEvent::SpeechStopped` (`ACTIVITY_END`), with the offset into the
  audio stream in `audio_start_ms` / `audio_end_ms`, so barge-in handlers work as on OpenAI.
- **Audio**: 16 kHz PCM16 in, 24 kHz PCM16 out.
- **Voices**: `Kore` and the other Gemini voices; `with_voice("Kore")`.
- **Language**: `with_language("en-US")` pins the spoken language
  (`speechConfig.languageCode`), so transcription does not switch languages on short or
  accented turns. Native-audio output models choose the language themselves and ignore it.
- **Session length**: `with_context_window_compression(None)` sends
  `contextWindowCompression` with a sliding window; `Some(n)` sets `triggerTokens`. Google
  documents that a session without it ends after about 2 minutes once video frames are sent
  and after about 15 minutes with audio only; with it only the connection limit of about 10
  minutes remains.
- **Auth**: `GEMINI_API_KEY` (or `GOOGLE_API_KEY`).

## Selecting a provider per session

Applications typically read the provider from a request and build the matching model. The
audio rates differ, so expose them too:

```rust
use adk_realtime::gemini::{GeminiLiveBackend, GeminiRealtimeModel};
use adk_realtime::model::BoxedModel;
use adk_realtime::openai::OpenAIRealtimeModel;
use std::sync::Arc;

#[derive(Clone, Copy)]
enum Provider {
    OpenAI,
    Gemini,
}

impl Provider {
    /// (input, output) sample rates in Hz.
    fn audio_rates(self) -> (u32, u32) {
        match self {
            Provider::OpenAI => (24_000, 24_000),
            Provider::Gemini => (16_000, 24_000),
        }
    }
}

/// Returns the model and its voice.
fn build_model(provider: Provider) -> anyhow::Result<(BoxedModel, &'static str)> {
    Ok(match provider {
        Provider::OpenAI => (
            Arc::new(OpenAIRealtimeModel::new(std::env::var("OPENAI_API_KEY")?, "gpt-realtime-2.1")),
            "marin",
        ),
        Provider::Gemini => (
            Arc::new(GeminiRealtimeModel::new(
                GeminiLiveBackend::studio(std::env::var("GEMINI_API_KEY")?),
                "models/gemini-3.1-flash-live-preview",
            )),
            "Kore",
        ),
    })
}
```

The web examples read `OPENAI_REALTIME_MODEL` and `GEMINI_REALTIME_MODEL`, so a model can be
pinned without recompiling.

## Capability matrix

| | OpenAI `gpt-realtime-2.1` | Gemini `3.1-flash-live-preview` | Gemini native audio |
|---|:---:|:---:|:---:|
| Voice (audio in and out) | Yes | Yes | Yes |
| Live transcripts | Yes | Yes | Yes |
| Server-side tools | Yes | Yes | Yes |
| Video frames | Yes (an `input_image` item per frame) | Yes (`realtimeInput.video`) | Yes (`realtimeInput.video`) |
| Affective dialogue | No | No | Yes |

## Diagnostics and payload privacy

Realtime frames carry transcripts, tool arguments, tool results, and identifiers. When an
OpenAI Realtime event of a known type fails to deserialize — provider schema drift — the
session yields `ServerEvent::Unknown` and logs a `warn` with a field-safe summary instead of
the frame:

| Field | Content |
|-------|---------|
| `event_type` | The provider event type that failed |
| `error` | The deserialization error |
| `payload.bytes` | Frame size in bytes |
| `payload.digest` | Short digest, for correlating repeats of the same drift |
| `payload.raw` | `<redacted>` unless payload recording is compiled in |

> **Note:** the digest groups log lines within a run. It is not a cryptographic digest and
> is not stable across processes.

To diagnose drift with the frame in hand, compile `adk-realtime` with the `record-payloads`
feature, which records the first 300 bytes:

```toml
[dependencies]
adk-realtime = { version = "3.0.0", features = ["openai", "record-payloads"] }
```

The feature is off by default; enabling it is a deliberate choice per build, since schema
drift is when operators widen log collection and retention.

Next: [Tools →](tools.md)
