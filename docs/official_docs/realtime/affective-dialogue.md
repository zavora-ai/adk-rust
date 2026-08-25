# Affective Dialogue

**Affective dialogue** makes the agent emotion-aware: it picks up the user's tone
from their voice — frustration, excitement, hesitation — and adapts its own
delivery to match. A frustrated customer hears a calmer, more careful agent; a
happy one hears warmth back. It's the difference between a script and a
conversation.

## Enabling it

One builder call on `RealtimeConfig`:

```rust
let config = RealtimeConfig::default()
    .with_instruction("You are a warm, empathetic support agent.")
    .with_voice("Kore")
    .with_audio_only()
    .with_vad(VadConfig::server_vad())
    .with_transcription()
    .with_affective_dialog(true);   // ← emotion-aware responses
```

## Requirements (read this)

Affective dialogue is a **Gemini-only** feature with two hard requirements:

1. **A native-audio model.** It is **not** supported on the half-cascade
   `gemini-3.1-flash-live-preview`. Use a native-audio model such as
   `models/gemini-live-2.5-flash-native-audio`.
2. **The `v1alpha` endpoint.** The crate's `GeminiLiveBackend::studio(...)`
   already connects over `v1alpha`, so this is handled for you.

If you set `with_affective_dialog(true)` on a model that doesn't support it, the
Gemini setup is rejected with *"Unknown name enableAffectiveDialog"* and the
session closes. So switch the model when you enable it:

```rust
let (default_model, voice) = if affective {
    ("models/gemini-live-2.5-flash-native-audio", "Kore")
} else {
    ("models/gemini-3.1-flash-live-preview", "Kore")
};
```

On OpenAI (and on non-native Gemini models) `with_affective_dialog(true)` is a
**no-op** — harmless, just ignored — so it's safe to set unconditionally and let
the provider decide.

## The trade-off

Native-audio models give the most natural, expressive voice and the affect
adaptation, but they **call tools less reliably** than the half-cascade model.
So it's a genuine choice:

- **Tool-heavy agent** (refunds, lookups, actions) → half-cascade
  (`gemini-3.1-flash-live-preview`), no affective dialogue.
- **Empathy-first agent** (support de-escalation, companionship, coaching) →
  native-audio + affective dialogue.

The [`customer_service`](examples.md#customer_service) example makes it opt-in for
exactly this reason: `CS_AFFECTIVE=1` switches Gemini to the native-audio model
and turns the flag on; otherwise it keeps the tool-reliable model.

## Under the hood

`with_affective_dialog(true)` sets `RealtimeConfig.affective_dialog`, and the
Gemini setup emits `enableAffectiveDialog: true` **inside `generationConfig`**
(not at the setup top level — the v1alpha endpoint rejects it there). You don't
need to know this to use it, but it's why the model + endpoint requirements are
strict.

> **Empathy without the flag.** Even on OpenAI or the half-cascade model, a
> well-written instruction ("notice the user's emotional state and acknowledge it
> sincerely") produces meaningfully empathetic behavior. Affective dialogue adds
> true *acoustic* tone-matching on top of that, on Gemini native-audio.

Next: [Memory →](memory.md)
