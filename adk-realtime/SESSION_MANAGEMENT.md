# Realtime Session Architecture & Context Mutation

This document explains how `adk-realtime` handles **mid-session context changes**—for example, swapping instructions or tools—without tearing down the upstream media transport (such as LiveKit over WebRTC). The design stays provider-agnostic even though provider realtime APIs behave differently. OpenAI supports live session updates with `session.update`, while Gemini Live uses **session resumption** to continue a session across connections. [OpenAI Realtime conversations](https://developers.openai.com/api/docs/guides/realtime-conversations), [Gemini Live session management](https://ai.google.dev/gemini-api/docs/live-session)

---

## Quick summary

- **Goal:** change the model’s active instructions/tools during a live voice session
- **OpenAI path:** update the existing session in place
- **Gemini path:** reconnect safely using session resumption
- **Runner job:** decide whether to apply now or queue a safe handoff

---

## 1. Why this exists

Voice agents often need to change behavior mid-call.

Examples:
- reception → billing
- triage → technical support
- general assistant → authenticated account workflow

To do that well, the system needs to update:
- **system instructions**
- **available tools**

without dropping the call or forcing the media transport to restart. OpenAI documents live session mutation through `session.update`. Gemini documents continuity through `sessionResumption` and `SessionResumptionUpdate`. [OpenAI Realtime conversations](https://developers.openai.com/api/docs/guides/realtime-conversations), [Gemini Live API reference](https://ai.google.dev/api/live)

---

## 2. Provider differences

### OpenAI Realtime
OpenAI supports updating an active realtime session with `session.update`. Most session properties can be changed during a session, though some fields have constraints, such as voice changes after audio output has started. OpenAI also uses `conversation.item.create` for messages and tool outputs. [OpenAI Realtime conversations](https://developers.openai.com/api/docs/guides/realtime-conversations), [OpenAI Realtime WebSocket guide](https://developers.openai.com/api/docs/guides/realtime-websocket)

### Gemini Live
Gemini Live models continuity differently. Its docs emphasize:
- enabling `sessionResumption` in setup
- receiving `SessionResumptionUpdate` messages
- reconnecting with `SessionResumptionConfig.handle`

Gemini also documents that a session is not always resumable at every moment, including while the model is generating or executing function calls. [Gemini Live session management](https://ai.google.dev/gemini-api/docs/live-session), [Gemini Live API reference](https://ai.google.dev/api/live)

**Result:** the orchestration layer cannot assume that every provider supports the same wire-level mutation flow.

---

## 3. `adk-realtime` strategy

`adk-realtime` separates:

1. **Intent** — “change the active cognitive context”
2. **Capability** — can this provider apply it live, or does it need a reconnect?
3. **Execution** — apply immediately, or queue a safe handoff

The higher-level app should request a context change once, without provider-specific branching.

---

## 4. Capability contract

Provider sessions return a semantic outcome:

```rust
pub enum ContextMutationOutcome {
    /// The provider updated the active session in place.
    Applied,

    /// The provider requires a reconnect/resumption with the new config.
    RequiresResumption(RealtimeConfig),
}
```

The `RealtimeRunner` manages this outcome. It either silently continues (if `Applied`) or gracefully queues a transport resumption, executing it only when the model's state machine confirms it is idle and safe to swap connections.
