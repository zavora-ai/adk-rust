# Realtime Providers

ADK-Rust speaks two realtime backends behind the same `RealtimeModel` interface,
so an application can offer both and switch per session. This page covers the
models, voices, endpoints, auth, and how to choose.

## OpenAI Realtime (GA)

```rust
use adk_realtime::openai::OpenAIRealtimeModel;
use adk_realtime::model::BoxedModel;
use std::sync::Arc;

let model: BoxedModel = Arc::new(OpenAIRealtimeModel::new(
    std::env::var("OPENAI_API_KEY")?,
    "gpt-realtime-2.1",
));
```

| Model | Use |
|-------|-----|
| `gpt-realtime-2.1` | Current production speech-to-speech model and default choice. |
| `gpt-realtime-translate` | Dedicated **translation** interpreter (different endpoint; see [Live Translation example](examples.md#live_translation)). |

- **Transport**: WebSocket (`openai` feature) or WebRTC (`openai-webrtc`, needs `cmake`).
- **Audio**: 24 kHz PCM16 in and out.
- **Voices**: `marin` (a natural GA voice), `alloy`, and others; set with
  `RealtimeConfig::with_voice("marin")`.
- **Auth**: `OPENAI_API_KEY`. The key lives on your server (never the browser).

## Gemini Live

```rust
use adk_realtime::gemini::{GeminiLiveBackend, GeminiRealtimeModel};

let model: BoxedModel = Arc::new(GeminiRealtimeModel::new(
    GeminiLiveBackend::studio(std::env::var("GEMINI_API_KEY")?),
    "models/gemini-3.1-flash-live-preview",
));
```

| Model | Use |
|-------|-----|
| `models/gemini-3.1-flash-live-preview` | Half-cascade live model. **Calls tools reliably** and accepts video frames. Default choice. |
| `models/gemini-live-2.5-flash-native-audio` | Native-audio model — the most natural voice, and the one that supports [affective dialogue](affective-dialogue.md). Weaker tool calling. |
| `models/gemini-3.5-live-translate-preview` | Dedicated **translation** model (see the [translation example](examples.md#live_translation)). |

- **Transport**: WebSocket (`gemini` feature, AI Studio) or Vertex AI Live
  (`vertex-live`, OAuth2 / ADC — see [Realtime Agents](../agents/realtime-agents.md#vertex-ai-live-google-cloud)).
- **Audio**: 16 kHz PCM16 **in**, 24 kHz PCM16 **out**.
- **Voices**: `Kore` and others; `with_voice("Kore")`.
- **Auth**: `GEMINI_API_KEY` (or `GOOGLE_API_KEY`).

> **Model names differ by endpoint.** AI Studio (API-key) uses names like
> `models/gemini-3.1-flash-live-preview`; Vertex/Agent Platform uses different
> names. The crate's `GeminiLiveBackend::studio(...)` targets AI Studio over the
> `v1alpha` endpoint (which is also what affective dialogue requires).

## Choosing a model

- **General voice + tools** → `gpt-realtime-2.1` or `gemini-3.1-flash-live-preview`.
  Both call tools reliably. Gemini is the better fit for **continuous video**.
- **Most natural voice / emotion-aware** → Gemini native-audio
  (`gemini-live-2.5-flash-native-audio`) with [affective dialogue](affective-dialogue.md) —
  at some cost to tool-calling reliability.
- **Reasoning-heavy** → `gpt-realtime-2.1`.
- **Translation** → the dedicated translate models (their own protocol).

## Selecting a provider per session

Applications typically read the provider from a request and build the matching
model. The audio rates differ, so expose them too:

```rust
#[derive(Clone, Copy)]
enum Provider { OpenAI, Gemini }

impl Provider {
    fn audio_rates(self) -> (u32, u32) {   // (input, output)
        match self {
            Provider::OpenAI => (24_000, 24_000),
            Provider::Gemini => (16_000, 24_000),
        }
    }
}

fn build_model(p: Provider) -> anyhow::Result<(BoxedModel, &'static str)> {
    Ok(match p {
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

The examples make all of these overridable with env vars
(`OPENAI_REALTIME_MODEL`, `GEMINI_REALTIME_MODEL`) so you can pin a model without
recompiling.

## Capability matrix

| | OpenAI `gpt-realtime-2.1` | Gemini `3.1-flash-live` | Gemini native-audio |
|---|:---:|:---:|:---:|
| Voice (audio in/out) | ✅ | ✅ | ✅ |
| Live transcripts | ✅ | ✅ | ✅ |
| Server-side tools | ✅ (reliable) | ✅ (reliable) | ⚠️ (weaker) |
| Video frames | ✅ (image items) | ✅ (continuous) | ✅ (continuous) |
| Affective dialogue | ❌ | ❌ | ✅ |

Next: [Tools →](tools.md)

## Diagnostics and payload privacy

Realtime frames carry transcripts, tool arguments, tool results, and identifiers. When a
recognized event fails to deserialize — provider schema drift — the warning reports a
field-safe summary and withholds the frame:

| Field | Content |
|-------|---------|
| `event_type` | The provider event type that failed |
| `error` | The deserialization error |
| `payload.bytes` | Frame size in bytes |
| `payload.digest` | Short digest, for correlating repeats of the same drift |
| `payload.raw` | `<redacted>` unless payload recording is compiled in |

> **Note:** the digest groups log lines within a run. It is not a cryptographic digest and
> is not stable across processes.

To diagnose drift with the frame in hand, compile `adk-realtime` with the
`record-payloads` feature, which records the first 300 bytes:

```toml
[dependencies]
adk-realtime = { version = "2.1.0", features = ["openai", "record-payloads"] }
```

The feature is off by default and is a deliberate choice per build, because schema drift is
exactly when operators widen log collection and retention.
