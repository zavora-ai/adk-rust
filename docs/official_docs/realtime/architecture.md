# Realtime Architecture

This page explains how a realtime session works in ADK-Rust: the layers, the event loop,
the audio pipeline, and the turn lifecycle.

## The four layers

```text
┌──────────────────────────────────────────────────────────────┐
│ IntegratedRealtimeRunner   (feature: integration)             │
│  • SessionService  → persists each completed turn             │
│  • MemoryService   → profile-card injection + turn storage    │
│  • EnhancedPluginManager → before/after-tool hooks           │
│  • ADK-tool bridge → run any `adk_core::Tool` in a session    │
└───────────────┬──────────────────────────────────────────────┘
                │ wraps
┌───────────────▼──────────────────────────────────────────────┐
│ RealtimeRunner                                                 │
│  • pulls ServerEvents from the session                         │
│  • on FunctionCallDone → executes the tool handler             │
│  • sends the tool result back, triggers the spoken answer      │
└───────────────┬──────────────────────────────────────────────┘
                │ drives
┌───────────────▼──────────────────────────────────────────────┐
│ RealtimeSession   (the live transport — a WebSocket)           │
│  send_audio / send_text / send_video_frame / send_tool_output  │
│  next_event() → ServerEvent stream                             │
└───────────────┬──────────────────────────────────────────────┘
                │ created by connect()
┌───────────────▼──────────────────────────────────────────────┐
│ RealtimeModel    OpenAIRealtimeModel | GeminiRealtimeModel     │
└──────────────────────────────────────────────────────────────┘
```

### `RealtimeModel` → `RealtimeSession`

A `RealtimeModel` is a thin factory. `OpenAIRealtimeModel::new(api_key, model_id)`
or `GeminiRealtimeModel::new(GeminiLiveBackend::studio(api_key), model_id)` build
one; `BoxedModel` is just `Arc<dyn RealtimeModel>`. Calling `connect()` opens the
WebSocket and returns a **`RealtimeSession`** — the object that actually speaks
the provider's wire protocol. You rarely touch the session directly; the runner
owns it. Its surface is small and provider-agnostic:

| Method | Purpose |
|--------|---------|
| `send_audio(&AudioChunk)`, `send_audio_base64(&str)` | Microphone audio |
| `send_text(&str)` | Typed input |
| `send_video_frame(mime_type, data_base64)` | A camera or screen frame; the default implementation ignores it |
| `send_tool_response(ToolResponse)`, `send_tool_output(ToolResponse)` | A tool result |
| `commit_audio`, `clear_audio`, `create_response`, `interrupt` | Turn control |
| `next_event`, `events` | The `ServerEvent` stream |
| `mutate_context(RealtimeConfig)` | Change the configuration mid-session: applied in place on OpenAI (`ContextMutationOutcome::Applied`); Gemini returns `RequiresResumption` |
| `close` | End the session |

Each provider implements this differently under the hood (OpenAI's
`input_audio_buffer.append` vs Gemini's `realtimeInput`), but the runner above
doesn't care.

### `RealtimeRunner`

Owns the session and runs the event loop. Its key job is **tool dispatch**: when
a `ServerEvent::FunctionCallDone` arrives, it looks up your handler, runs it, and
sends the result back (see [Tools](tools.md)). It exposes the session's verbs
plus tool registration:

```rust
use adk_realtime::RealtimeRunner;

async fn session(runner: &RealtimeRunner, pcm16_base64: &str, jpeg_base64: &str) -> adk_realtime::Result<()> {
    runner.connect().await?;
    runner.send_audio(pcm16_base64).await?; // microphone audio
    runner.send_video_frame("image/jpeg", jpeg_base64).await?;
    runner.send_text("What do you see?").await?; // typed input
    runner.create_response().await?; // text input needs an explicit trigger
    while let Some(event) = runner.next_event().await {
        let _event = event?; // the next ServerEvent
    }
    runner.close().await
}
```

### `IntegratedRealtimeRunner`

The application layer. It wraps a `RealtimeRunner` and, as events flow, wires in:

- **`SessionService`** — completed turns are appended to the session history.
- **`MemoryService`** — queried at connect (for context) and written to per turn
  (configurable). See [Memory](memory.md).
- **`EnhancedPluginManager`** — tool calls pass through `before_tool_call` /
  `after_tool_call` hooks.
- **The ADK-tool bridge** — `.adk_tool(Arc<dyn Tool>)` lets any normal
  `adk_core::Tool` (e.g. `adk-tool`'s built-ins) run in a realtime session via a
  synthesized `ToolContext` scoped to the session identity.

You build it with a builder:

```rust
use adk_core::Tool;
use adk_memory::MemoryService;
use adk_realtime::config::{RealtimeConfig, ToolDefinition};
use adk_realtime::events::ToolCall;
use adk_realtime::integration::{IntegratedRealtimeRunner, IntegrationConfig};
use adk_realtime::model::BoxedModel;
use adk_realtime::runner::FnToolHandler;
use adk_session::SessionService;
use serde_json::json;
use std::sync::Arc;

fn build(
    model: BoxedModel,
    config: RealtimeConfig,
    sessions: Arc<dyn SessionService>,
    memory: Arc<dyn MemoryService>,
    remember_tool: Arc<dyn Tool>,
) -> adk_realtime::Result<IntegratedRealtimeRunner> {
    let weather = ToolDefinition {
        name: "get_weather".into(),
        description: Some("Current weather for a city.".into()),
        parameters: Some(json!({"type": "object", "properties": {"city": {"type": "string"}}})),
    };
    IntegratedRealtimeRunner::builder()
        .model(model)
        .config(config)
        .identity("app", "user", "session-id") // required
        .session_service(sessions) // optional
        .memory_service(memory) // optional
        .integration_config(IntegrationConfig::default())
        // A native realtime `ToolHandler`.
        .tool(weather, FnToolHandler::new(|_call: &ToolCall| Ok(json!({"forecast": "sunny"}))))
        // A bridged `adk_core::Tool`.
        .adk_tool(remember_tool)
        .build()
}
```

`IntegrationConfig` controls the automatic behaviours; these are its defaults:

| Field | Default | Effect |
|-------|---------|--------|
| `persist_transcripts` | `true` | Append each completed turn to the session |
| `store_to_memory` | `true` | `memory_service.add_session` per turn |
| `inject_memory_context` | `true` | Query memory at connect |
| `max_memory_injection` | `10` | Recalled entries carried into the instruction |
| `max_history_injection` | `20` | Prior turns carried into the instruction (see [Memory](memory.md#what-is-carried-into-a-resumed-session)) |

## The server-side bridge (web apps)

Browsers can't hold the provider WebSocket securely — your API key would leak,
and the audio/event plumbing belongs server-side. So the recommended topology is
a **server-side bridge**: the browser is a thin audio/video device, and your Rust
server owns the realtime session.

```text
  browser ──mic PCM16 + camera JPEG (base64 over your WS)──▶  your Axum /ws
  browser ◀──agent PCM16 + transcripts + tool events──────    IntegratedRealtimeRunner ──▶ provider
```

The API key never reaches the browser; tools run on your server. Every web
example in this section uses this pattern — see
[Building web apps](building-web-apps.md) for the full protocol and the Web Audio
code.

## The audio pipeline

Realtime audio is **raw PCM16, mono, little-endian** — no containers. The only
thing that varies is the sample rate, and it varies **per provider and per
direction**:

| Provider | Input (mic → model) | Output (model → you) |
|----------|--------------------:|---------------------:|
| OpenAI `gpt-realtime-2.1` | 24 kHz | 24 kHz |
| Gemini Live | 16 kHz | 24 kHz |

Because the rates differ, a bridge **negotiates them to the browser** before any
audio flows (the examples send a `ready` message with `input_rate`/`output_rate`,
and the browser creates its capture/playback `AudioContext`s at those rates).
Audio crosses your WebSocket base64-encoded; `ServerEvent::AudioDelta` carries
decoded PCM16 bytes you re-encode for the browser to play gaplessly.

## The turn lifecycle

A "turn" is one exchange. With **server VAD** the provider detects speech
boundaries and responds automatically; you don't call `create_response()` for
audio. A typical voice turn produces this event sequence:

```text
SpeechStarted                 → user began talking (flush any playing audio = barge-in)
InputTranscriptDelta…         → live transcript of what the user is saying
SpeechStopped                 → user finished
  (model thinks)
TranscriptDelta…              → the agent's spoken answer, as text
AudioDelta…                   → the agent's spoken answer, as PCM16
ResponseDone                  → turn complete
```

For **text input** (a chat box), there's no VAD trigger, so you must call
`create_response()` after `send_text()` to ask the model to reply.

### Tool turns span two responses

When the model calls a tool, the turn is longer:

```text
(maybe a short spoken preamble) + FunctionCallDone(name, args)
ResponseDone                  ← the "dispatch" response ends here
  → runner executes your handler, sends the result back,
    and triggers ONE follow-up response
TranscriptDelta… / AudioDelta…  ← the spoken answer using the tool result
ResponseDone                  ← turn truly complete
```

This is why a UI should treat a turn as finished only on a `ResponseDone` that
did **not** contain a tool call. ADK-Rust issues exactly **one** follow-up
`response.create` per turn even when several tools are called at once — see
[Tools](tools.md#parallel-tool-calls).

## Server events you'll handle

`ServerEvent` is the provider-agnostic event enum the runner yields. The ones you
typically render:

| Event | Meaning |
|-------|---------|
| `AudioDelta { delta, .. }` | PCM16 bytes of the agent's speech — play them |
| `TranscriptDelta { delta, .. }` | The agent's spoken answer, as text |
| `InputTranscriptDelta { delta, .. }` | Live transcript of the **user's** speech (streamed) |
| `InputTranscriptCompleted { transcript, .. }` | Final user transcript (OpenAI sends one) |
| `SpeechStarted` / `SpeechStopped` | VAD detected the user starting/stopping |
| `FunctionCallDone { name, arguments, call_id, .. }` | The model wants a tool (`arguments` is a JSON string) |
| `ResponseDone { .. }` | A response finished |
| `TextDelta { delta, .. }` | Non-spoken text (e.g. Gemini "thinking") — usually not shown |
| `Error { error, .. }` | Provider error |

> `#[non_exhaustive]`: always include a `_ => {}` arm when matching `ServerEvent`.

Next: [Providers →](providers.md)
