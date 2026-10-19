# Realtime & Multimodal Agents

Build agents that **talk, listen, see, and act** in real time — over a live,
low-latency connection to OpenAI's Realtime API or Google's Gemini Live API.

A realtime agent holds an open session: the user's microphone streams up continuously,
the model streams audio and a live transcript back, detects when the user starts and stops
speaking, can be interrupted mid-sentence (barge-in), can see frames from a camera, and can
call your tools and speak the result, all within one connection.

> **New to ADK-Rust?** Read the [Introduction](../introduction.md) and
> [Quickstart](../quickstart.md) first. This section assumes you know what an
> agent, tool, and session are.

## What you can build

| Capability | What it means | Page |
|------------|---------------|------|
| **Voice conversations** | Bidirectional PCM16 audio with VAD and barge-in | [Architecture](architecture.md) |
| **Two providers, one API** | OpenAI Realtime or Gemini Live, swappable per session | [Providers](providers.md) |
| **Server-side tools** | The model calls `process_refund(...)`; your Rust runs it under the run's tool policy and the model speaks the result | [Tools](tools.md) |
| **Multimodal vision** | Stream camera frames — the agent sees what the user shows it | [Multimodal](multimodal.md) |
| **Affective dialogue** | The agent reads the user's emotional tone and adapts (Gemini native-audio) | [Affective dialogue](affective-dialogue.md) |
| **Long-horizon memory** | A knowledge graph the agent reads at session start and curates as it learns | [Memory](memory.md) |
| **Web apps** | A browser front-end (mic + camera) bridged through your server | [Building web apps](building-web-apps.md) |

Then see the four runnable [example apps](examples.md).

## The mental model

There are two layers, and most applications use the top one:

```text
  IntegratedRealtimeRunner          ← sessions, memory, tools, plugins (use this)
        │ wraps
  RealtimeRunner                    ← event loop + tool dispatch
        │ drives
  RealtimeSession  (a transport)    ← the live WebSocket to the provider
        │ created by
  RealtimeModel    (OpenAI | Gemini)
```

- A **`RealtimeModel`** (e.g. `OpenAIRealtimeModel`, `GeminiRealtimeModel`)
  knows how to `connect()` and produce a **`RealtimeSession`** — the live
  transport.
- **`RealtimeRunner`** owns that session: it pumps events, and when the model
  asks for a tool it executes the handler and sends the result back.
- **`IntegratedRealtimeRunner`** wraps the runner and connects it to the rest of
  ADK — `SessionService` (transcript persistence), `MemoryService` (recall +
  storage), `EnhancedPluginManager` (hooks), and a bridge that lets any
  `adk-core` `Tool` run in a realtime session. The web examples use it.

(There is also a higher-level [`RealtimeAgent`](../agents/realtime-agents.md) for
simple voice-only agents, and direct transports — WebRTC, LiveKit, Vertex AI
Live — documented there.)

## Install

```toml
[dependencies]
adk-realtime = { version = "3.0.0", features = ["openai", "gemini", "integration"] }
adk-session = "3.0.0"
anyhow = "1"
tokio = { version = "1", features = ["full"] }
```

| Feature | Adds |
|---------|------|
| `openai` | OpenAI Realtime (WebSocket) |
| `gemini` | Gemini Live (WebSocket) |
| `integration` | `IntegratedRealtimeRunner` — sessions, memory, plugins, tool bridge |
| `openai-webrtc` | OpenAI over WebRTC (implies `openai`; needs `cmake`) |
| `vertex-live` | Gemini via Vertex AI Live (OAuth2 / ADC; implies `gemini`) |
| `livekit` | LiveKit WebRTC bridge |
| `heygen-avatar`, `did-avatar` | Video avatar providers |
| `full` | `openai`, `gemini`, `vertex-live`, and `livekit` |
| `full-webrtc` | `full` and `openai-webrtc` |
| `record-payloads` | Bounded raw provider frames in diagnostics (see [Providers](providers.md#diagnostics-and-payload-privacy)) |

## Quick start

A headless turn: connect, ask by text, and pump the streamed reply.

```rust
use adk_realtime::config::{RealtimeConfig, VadConfig};
use adk_realtime::events::ServerEvent;
use adk_realtime::integration::{IntegratedRealtimeRunner, IntegrationConfig};
use adk_realtime::model::BoxedModel;
use adk_realtime::openai::OpenAIRealtimeModel;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let model: BoxedModel = Arc::new(OpenAIRealtimeModel::new(
        std::env::var("OPENAI_API_KEY")?,
        "gpt-realtime-2.1",
    ));

    let config = RealtimeConfig::default()
        .with_instruction("You are a friendly assistant. Keep replies short.")
        .with_voice("marin")
        .with_audio_only()
        .with_vad(VadConfig::server_vad()) // the provider decides turn boundaries
        .with_transcription(); // also transcribe the user's audio

    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "demo".into(),
            user_id: "u1".into(),
            session_id: Some("s1".into()),
            state: Default::default(),
        })
        .await?;

    let runner = IntegratedRealtimeRunner::builder()
        .model(model)
        .config(config)
        .identity("demo", "u1", "s1")
        .session_service(sessions)
        .integration_config(IntegrationConfig::default())
        .build()?;

    runner.connect().await?;
    runner.send_text("Say hello in one short sentence.").await?;
    runner.create_response().await?; // text input needs an explicit trigger

    while let Some(event) = runner.next_event().await {
        match event? {
            ServerEvent::TranscriptDelta { delta, .. } => print!("{delta}"),
            ServerEvent::AudioDelta { .. } => { /* PCM16 to play */ }
            ServerEvent::ResponseDone { .. } => break,
            _ => {}
        }
    }
    runner.close().await?;
    Ok(())
}
```

In an application you stream microphone audio with `runner.send_audio(base64_pcm16)`
instead of `send_text`, and server VAD starts each response, so no `create_response()` is
needed. See [Building web apps](building-web-apps.md).

## Where to go next

1. **[Architecture](architecture.md)** — how the pieces fit, the event loop, the audio pipeline.
2. **[Providers](providers.md)** — OpenAI vs Gemini models, voices, endpoints, auth.
3. **[Tools](tools.md)** — server-side function calling.
4. **[Multimodal](multimodal.md)** — sending video frames.
5. **[Affective dialogue](affective-dialogue.md)** — emotion-aware responses.
6. **[Memory](memory.md)** — knowledge-graph long-term memory.
7. **[Building web apps](building-web-apps.md)** — the browser bridge.
8. **[Examples](examples.md)** — four runnable apps.
