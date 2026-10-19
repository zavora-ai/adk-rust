# Affective Dialogue

**Affective dialogue** makes a Gemini Live native-audio model emotion-aware: it picks up the
user's tone of voice — frustration, excitement, hesitation — and adapts its own delivery.

## Enabling it

One builder call on `RealtimeConfig`:

```rust
use adk_realtime::config::{RealtimeConfig, VadConfig};

fn config() -> RealtimeConfig {
    RealtimeConfig::default()
        .with_instruction("You are a warm, empathetic support agent.")
        .with_voice("Kore")
        .with_audio_only()
        .with_vad(VadConfig::server_vad())
        .with_transcription()
        .with_affective_dialog(true) // emotion-aware responses
}
```

`with_affective_dialog(true)` sets `RealtimeConfig::affective_dialog`, and the Gemini setup
sends `enableAffectiveDialog: true` inside `generationConfig`.

## Requirements

| Requirement | Detail |
|-------------|--------|
| A Gemini Live native-audio model | On AI Studio, `models/gemini-2.5-flash-native-audio-preview-12-2025` (the catalog lists it as deprecated); only native-audio models honor the setting, and the half-cascade `models/gemini-3.1-flash-live-preview` is not one |
| The `v1alpha` endpoint | `GeminiLiveBackend::studio(...)` connects to it |

On OpenAI the setting is ignored, so it can be set for every provider. Choose the model with
it:

```rust
use adk_realtime::gemini::{GeminiLiveBackend, GeminiRealtimeModel};
use adk_realtime::model::BoxedModel;
use std::sync::Arc;

fn gemini_model(api_key: String, affective: bool) -> BoxedModel {
    let model_id = if affective {
        "models/gemini-2.5-flash-native-audio-preview-12-2025"
    } else {
        "models/gemini-3.1-flash-live-preview"
    };
    Arc::new(GeminiRealtimeModel::new(GeminiLiveBackend::studio(api_key), model_id))
}
```

## In the examples

The [`customer_service`](examples.md#customer_service--aria) example makes it opt-in:
`CS_AFFECTIVE=1` switches Gemini to the native-audio model and turns the flag on; otherwise
it keeps `models/gemini-3.1-flash-live-preview`, which the example uses for its tools.

An instruction that asks the model to notice and acknowledge the user's emotional state
works on every model; affective dialogue adds tone-matching from the audio itself.

Next: [Memory →](memory.md)
