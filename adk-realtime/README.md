# adk-realtime

Real-time bidirectional audio streaming for Rust Agent Development Kit (ADK-Rust) agents.

[![Crates.io](https://img.shields.io/crates/v/adk-realtime.svg)](https://crates.io/crates/adk-realtime)
[![Documentation](https://docs.rs/adk-realtime/badge.svg)](https://docs.rs/adk-realtime)
[![License](https://img.shields.io/crates/l/adk-realtime.svg)](../LICENSE)

## Overview

`adk-realtime` connects voice agents to real-time streaming APIs. It offers a low-level session
API (`RealtimeModel`, `RealtimeSession`), an event loop with tool dispatch (`RealtimeRunner`), and
`RealtimeAgent`, which implements `adk_core::Agent` so a voice agent runs under the standard ADK
runner.

## Features

- **RealtimeAgent** — implements `adk_core::Agent` with instructions, tools, sub-agent handoff,
  and agent and tool callbacks
- **Providers** — OpenAI Realtime API, Gemini Live API, Vertex AI Live API
- **Transports** — WebSocket, WebRTC (OpenAI), and a LiveKit bridge
- **Audio** — PCM16 and G.711 (µ-law, A-law) encodings; Opus on the WebRTC transport
- **Voice activity detection** — server-side VAD, and `Manual` or `Automatic` interruption handling
- **Governed tool calls** — every tool call goes through `adk_core::authorize_tool_call`: the kill
  switch, the `ToolPolicy`, and confirmation
- **Context mutation** — swap instructions and tools mid-session (OpenAI: in-place update; Gemini:
  session resumption)
- **Video avatars** — HeyGen and D-ID providers (`heygen-avatar`, `did-avatar`)
- **Opt-in dependencies** — no provider or transport is compiled by default

## Architecture

```text
              ┌─────────────────────────────────────────┐
              │              Agent trait                │
              │  (name, description, run, sub_agents)   │
              └────────────────┬────────────────────────┘
                               │
       ┌───────────────────────┼───────────────────────┐
       │                       │                       │
┌──────▼──────┐      ┌─────────▼─────────┐   ┌─────────▼─────────┐
│  LlmAgent   │      │  RealtimeAgent    │   │  SequentialAgent  │
│ (text-based)│      │  (voice-based)    │   │   (workflow)      │
└─────────────┘      └───────────────────┘   └───────────────────┘

┌──────────────────────────────────────────────────────────────┐
│                    RealtimeSession trait                     │
├──────────────┬──────────────┬──────────────┬─────────────────┤
│ OpenAI WS    │ OpenAI WebRTC│ Gemini Live  │ Vertex AI Live  │
│ (openai)     │ (openai-     │ (gemini)     │ (vertex-live)   │
│              │  webrtc)     │              │                 │
└──────────────┴──────────────┴──────────────┴─────────────────┘
```

## Providers and transports

| Provider | Model | Transport | Feature |
|----------|-------|-----------|---------|
| OpenAI | `gpt-realtime-2.1` | WebSocket | `openai` |
| OpenAI | `gpt-realtime-2.1` | WebRTC with Opus | `openai-webrtc` |
| Google Gemini Live | `gemini-3.1-flash-live-preview` | WebSocket | `gemini` |
| Vertex AI Live | for example `gemini-live-2.5-flash-native-audio` | WebSocket with OAuth2 (ADC) | `vertex-live` |
| LiveKit | any of the above | WebRTC bridge | `livekit` |

## Quick Start

```toml
[dependencies]
adk-realtime = { version = "3.0.0", features = ["openai"] }
```

### Low-level session

The program sends one text turn and prints the text reply.

```rust
use adk_realtime::openai::OpenAIRealtimeModel;
use adk_realtime::{RealtimeConfig, RealtimeModel, ServerEvent};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = OpenAIRealtimeModel::new(std::env::var("OPENAI_API_KEY")?, "gpt-realtime-2.1");

    let config = RealtimeConfig::default()
        .with_instruction("You are a helpful assistant. Answer in one sentence.")
        .with_modalities(vec!["text".to_string()]);

    let session = model.connect(config).await?;
    session.send_text("Name one planet.").await?;
    session.create_response().await?;

    while let Some(event) = session.next_event().await {
        match event? {
            ServerEvent::TextDelta { delta, .. } => print!("{delta}"),
            ServerEvent::AudioDelta { .. } => { /* play audio */ }
            ServerEvent::ResponseDone { .. } => break,
            _ => {}
        }
    }
    println!();
    session.close().await?;
    Ok(())
}
```

### RealtimeAgent

`RealtimeAgent` implements `adk_core::Agent`, so `adk_runner::Runner` runs it like any other agent.

```rust
use adk_realtime::RealtimeAgent;
use adk_realtime::openai::OpenAIRealtimeModel;
use std::sync::Arc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = Arc::new(OpenAIRealtimeModel::new(std::env::var("OPENAI_API_KEY")?, "gpt-realtime-2.1"));

    let agent = RealtimeAgent::builder("voice_assistant")
        .model(model)
        .instruction("You are a helpful voice assistant.")
        .voice("alloy")
        .server_vad()
        .build()?;

    println!("built {}", adk_core::Agent::name(&agent));
    Ok(())
}
```

`RealtimeAgentBuilder::build` fails with `agent.duplicate_tool_name` when two registered tools
share a name.

## Transport guides

### Gemini Live

```toml
adk-realtime = { version = "3.0.0", features = ["gemini"] }
```

```rust
use adk_realtime::gemini::{GeminiLiveBackend, GeminiRealtimeModel};
use adk_realtime::{RealtimeConfig, RealtimeModel};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let backend = GeminiLiveBackend::studio(std::env::var("GOOGLE_API_KEY")?);
    let model = GeminiRealtimeModel::new(backend, "models/gemini-3.1-flash-live-preview");

    let session = model.connect(RealtimeConfig::default().with_instruction("You are concise.")).await?;
    session.close().await?;
    Ok(())
}
```

The `gemini` feature compiles in more than one rustls crypto provider, so `connect` installs
`aws-lc-rs` as the process-level provider through `adk_core::ensure_crypto_provider()`; a
provider the application installed first is kept.
Gemini Live takes 16 kHz mono PCM16 input and returns 24 kHz mono PCM16 output.

### Vertex AI Live

```toml
adk-realtime = { version = "3.0.0", features = ["vertex-live"] }
```

```rust
use adk_realtime::gemini::{GeminiLiveBackend, GeminiRealtimeModel};
use adk_realtime::{RealtimeConfig, RealtimeModel};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Application Default Credentials, for example from `gcloud auth application-default login`.
    let backend = GeminiLiveBackend::vertex_adc(std::env::var("GOOGLE_CLOUD_PROJECT")?, "us-central1")?;
    let model = GeminiRealtimeModel::new(backend, "gemini-live-2.5-flash-native-audio");

    let session = model.connect(RealtimeConfig::default()).await?;
    session.close().await?;
    Ok(())
}
```

`GeminiLiveBackend::Vertex { credentials, region, project_id }` takes credentials you built
yourself, and `gemini::build_vertex_live_url(region, project_id)` returns the endpoint.

### OpenAI WebRTC

```toml
adk-realtime = { version = "3.0.0", features = ["openai-webrtc"] }
```

```rust
use adk_realtime::openai::{OpenAIRealtimeModel, OpenAITransport};
use adk_realtime::{RealtimeConfig, RealtimeModel};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = OpenAIRealtimeModel::new(std::env::var("OPENAI_API_KEY")?, "gpt-realtime-2.1")
        .with_transport(OpenAITransport::WebRTC);

    let session = model.connect(RealtimeConfig::default()).await?;
    session.close().await?;
    Ok(())
}
```

`openai-webrtc` builds the Opus C library from source through `audiopus`, so `cmake` must be
installed. With cmake 4.0 or later, set `CMAKE_POLICY_VERSION_MINIMUM=3.5`.

### LiveKit bridge

```toml
adk-realtime = { version = "3.0.0", features = ["livekit", "openai"] }
```

`LiveKitRoomBuilder` is a typestate builder: `connect()` is available once `identity` is set.
`LiveKitConfig` keeps the API key and secret as `secrecy::SecretString` and redacts them in
`Debug` output.

```rust
use adk_realtime::livekit::{LiveKitConfig, LiveKitRoomBuilder};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = LiveKitConfig::new(
        "wss://your-server.livekit.cloud",
        std::env::var("LIVEKIT_API_KEY")?,
        std::env::var("LIVEKIT_API_SECRET")?,
    )?;

    let bundle = LiveKitRoomBuilder::new(config)
        .identity("my-agent")
        .name("Voice Agent")
        .room_name("session-room-123")
        .with_audio(24_000, 1)
        .connect()
        .await?;

    println!("joined {}", bundle.room.name());
    Ok(())
}
```

`LiveKitEventHandler::new(inner, audio_source, sample_rate, channels)` wraps an `EventHandler`
and publishes model audio to the room. `bridge_input(track, &runner)` feeds a participant's audio
track into a `RealtimeRunner`; `bridge_gemini_input` does the same at Gemini's 16 kHz.

## Mid-session context mutation

`RealtimeRunner::update_session` swaps instructions and tools without dropping the call:

```rust
use adk_realtime::RealtimeRunner;
use adk_realtime::config::{RealtimeConfig, SessionUpdateConfig};

async fn become_billing_agent(runner: &RealtimeRunner) -> adk_realtime::Result<()> {
    let update = SessionUpdateConfig(
        RealtimeConfig::default().with_instruction("You are now a billing specialist."),
    );
    runner.update_session(update).await
}
```

| Provider | Outcome |
|----------|---------|
| OpenAI | Updates the session in place with `session.update` (`ContextMutationOutcome::Applied`) |
| Gemini | Queues a transport resumption that runs when the session is idle (`ContextMutationOutcome::RequiresResumption`); a newer update replaces a pending one |

See [SESSION_MANAGEMENT.md](SESSION_MANAGEMENT.md) and the `openai_session_update` and
`gemini_context_mutation` examples.

## Interruption detection

```rust
use adk_realtime::{InterruptionDetection, RealtimeConfig};

fn main() {
    let config = RealtimeConfig::default().with_interruption_detection(InterruptionDetection::Automatic);
    assert_eq!(config.interruption_detection, Some(InterruptionDetection::Automatic));
}
```

- `Manual` (default): the application cancels responses explicitly.
- `Automatic`: VAD detects the start of user speech and cancels the agent's audio output.

## Tool governance

`RealtimeAgent`, `RealtimeRunner`, and `IntegratedRealtimeRunner` (`integration` feature) dispatch
every tool call through `adk_core::authorize_tool_call`. `RealtimeAgent` reads the kill switch,
`ToolPolicy`, and confirmation from the run's `RunConfig` and runs the runner's plugin callbacks;
the runners take them from `tool_governance` on their builders.

| Behaviour | Rule |
|-----------|------|
| Tool timeout | `tool_timeout(duration)` answers a call that runs too long with an error; the default is five minutes |
| Malformed arguments | Arguments that are not valid JSON are answered with an error and the tool does not run |
| Tool and callback errors | The model receives the error's component, category, and code; `expose_tool_error_messages(true)` sends the full message |
| After-tool failure | An after-tool callback or plugin error withholds the result (`result_withheld`) |
| Result screening | `tool_result_screen(screen)` rewrites or withholds each result before the model sees it; a screen that cannot decide withholds the result |
| Authenticated identity | `IntegratedRealtimeRunnerBuilder::request_context(ctx)` runs every tool call as the identity the server authenticated: tool contexts answer `is_authenticated()` with `true` and `user_scopes()` with its scopes. Without it they report an unauthenticated user; a context for a user other than `identity`'s fails `build` |
| Run config in tool contexts | With `tool_governance`, every tool context answers `run_config()` with the governance config and `acting_context()` with the acting context the gate authorized the call under |
| Governing agent | `agent_name(name)` on either runner builder names the agent calls are governed, metered and held under (default `realtime`); a surface running an existing agent's tools sets that agent's name, so its tool policy recognises them |
| Held calls | A call the governed path holds, or that a tool's delegate holds (an `AgentTool` whose agent held a call), is answered to the model as `confirmation_pending`, recorded in the session under the runner's agent name (`agent_name`, default `realtime`) and in the run's `ApprovalStore`, and surfaced as `ServerEvent::ToolConfirmationRequested`; `held_calls()` lists them and `resume_held_calls()` runs each one that has a decision under its original call ID. `RealtimeAgent` holds its own call when its delegate holds one and resumes decided calls at the start of a run |
| Approval binding | A held call's request names the runner's acting agent and the tool's origin: an ADK tool's `Tool::origin()`, or for a native handler `REALTIME_HANDLER_ORIGIN` (`realtime`) with a digest of its `ToolDefinition` (`ToolDefinition::handler_origin`), so an approval never covers another agent's call, a `Tool` of the same name, or a handler redefined since |
| Concurrent dispatch | `IntegratedRealtimeRunnerBuilder::concurrent_tool_dispatch(true)` runs each dispatch on its own task, so `next_event` keeps reading provider events while a tool runs; off by default |
| Metering | With a `spend_ledger` in the governance config, every response reserves a hold (the run's `LlmSpendEstimate`) when it starts and commits its cost when it ends: `RealtimeUsage::from_response` reads OpenAI's `response.usage` or Gemini's `usageMetadata`, `realtime_pricing(RealtimePricing)` prices it, and without pricing the estimate is committed. A refused hold ends the session with `RealtimeError::SpendRefused`. `RealtimeAgent` yields the usage on an event at each response's end |
| Output guardrails | `output_guardrails(GuardrailSet)` (feature `guardrails`) screens each completed turn as `LlmAgent` screens final text: a transform replaces the persisted text; a block interrupts the response, persists nothing of the turn and reports `RealtimeError::OutputBlocked` (the agent ends the run with the guardrail's error) |

## RealtimeAgent builder

| Method | Description |
|--------|-------------|
| `instruction(str)`, `instruction_provider(fn)` | Static or dynamic system instruction |
| `global_instruction(str)`, `global_instruction_provider(fn)` | Global instruction, prepended |
| `tool(Arc<dyn Tool>)`, `toolset(Arc<dyn Toolset>)` | Register tools |
| `sub_agent(Arc<dyn Agent>)` | Register a sub-agent for handoff |
| `before_agent_callback`, `after_agent_callback` | Agent callbacks |
| `before_tool_callback`, `after_tool_callback` | Tool callbacks |
| `tool_timeout`, `tool_result_screen`, `expose_tool_error_messages` | See [Tool governance](#tool-governance) |
| `voice(str)` | Voice, such as `"alloy"` |
| `server_vad()`, `vad(VadConfig)` | Server-side VAD with defaults, or custom VAD |
| `modalities(vec)` | Output modalities, such as `["audio"]` or `["text"]`. Unset, the provider's default applies: audio with a transcript on OpenAI and Gemini Live |
| `transcription()` | Transcribe the user's speech (and, on Gemini Live, the model's), so spoken turns are saved |
| `on_audio`, `on_transcript`, `on_speech_started`, `on_speech_stopped` | Event callbacks |
| `avatar(AvatarConfig)`, `avatar_provider(provider)` | Video avatar (`video-avatar`) |

Under `Runner`, the agent saves each of the user's spoken turns (with `transcription()`) as a
user event ahead of the model's reply: on OpenAI when its transcript completes, on Gemini Live,
which never completes a transcript, when the model's reply starts or ends. Each reply is saved
under its own event id, also on Gemini Live, which streams replies without an item id. A
session the provider closes with a failure code — Gemini Live closes with 1007 on a setup it
refuses — ends the run with `model.realtime.session_closed`, carrying the code and the
provider's reason.

## Events

| Server event | Description |
|--------------|-------------|
| `SessionCreated`, `SessionUpdated` | Session established or updated |
| `AudioDelta`, `AudioDone` | Output audio chunk, end of output audio |
| `TextDelta`, `TextDone` | Text response chunk, end of text |
| `TranscriptDelta`, `TranscriptDone` | Transcript of the output audio |
| `InputTranscriptDelta`, `InputTranscriptCompleted` | Transcript of the user's input audio |
| `FunctionCallDelta`, `FunctionCallDone` | Tool call arguments, completed tool call |
| `ResponseCreated`, `ResponseDone` | Response started, response completed |
| `ToolConfirmationRequested` | A tool call held for a confirmation decision (emitted by the integrated runner, not a provider) |
| `SpeechStarted`, `SpeechStopped` | VAD detected speech or silence (OpenAI `speech_started` / `speech_stopped`, Gemini Live `voiceActivity`) |
| `Error` | Provider error |
| `Unknown` | An event type this version does not model |

| Client event | Description |
|--------------|-------------|
| `AudioDelta` | Append audio to the input buffer |
| `InputAudioBufferCommit`, `InputAudioBufferClear` | Commit or clear the input buffer |
| `ConversationItemCreate` | Send a text message or tool response |
| `ResponseCreate`, `ResponseCancel` | Request or cancel a response |
| `SessionUpdate` | Update the session configuration |

## Audio formats

| Encoding | Sample rate | Provider |
|----------|-------------|----------|
| PCM16 mono | 24 kHz | OpenAI input and output, Gemini output |
| PCM16 mono | 16 kHz | Gemini input |
| G.711 µ-law, A-law | 8 kHz | OpenAI |
| Opus | 24 kHz mono | OpenAI WebRTC |

## Feature flags

| Feature | Enables | Requires |
|---------|---------|----------|
| `openai` | OpenAI Realtime API over WebSocket | |
| `gemini` | Gemini Live API over WebSocket | |
| `vertex-live` | Vertex AI Live API with ADC (implies `gemini`) | GCP credentials |
| `openai-webrtc` | OpenAI WebRTC transport with Opus (implies `openai`) | cmake |
| `livekit` | LiveKit WebRTC bridge | a LiveKit server |
| `video-avatar` | Avatar configuration and the `AvatarProvider` trait | |
| `heygen-avatar` | HeyGen LiveAvatar provider (implies `video-avatar`, `livekit`) | |
| `did-avatar` | D-ID Realtime Agents provider (implies `video-avatar`) | |
| `integration` | `IntegratedRealtimeRunner`: sessions, memory, and plugins around `RealtimeRunner` | |
| `guardrails` | Output guardrails on the model's text and transcripts (`adk-guardrail`) | |
| `scripted` | `ScriptedRealtimeModel`: a provider-free model that answers each client action from a script, for tests | |
| `record-payloads` | Quotes provider frames in OpenAI schema-drift warnings; without it they are redacted | |
| `full` | `openai`, `gemini`, `vertex-live`, `livekit` | |
| `full-webrtc` | `full` and `openai-webrtc` | cmake |

No feature is enabled by default.

## Examples

```bash
# OpenAI mid-session context mutation
cargo run -p adk-realtime --example openai_session_update --features openai

# Gemini mid-session context mutation (session resumption)
cargo run -p adk-realtime --example gemini_context_mutation --features gemini

# Vertex AI Live voice assistant and tools (ADC and a GCP project)
cargo run -p adk-realtime --example vertex_live_voice --features vertex-live
cargo run -p adk-realtime --example vertex_live_tools --features vertex-live

# LiveKit bridge with OpenAI or Gemini (a LiveKit server)
cargo run -p adk-realtime --example livekit_bridge --features "livekit,openai"
cargo run -p adk-realtime --example livekit_gemini_bridge --features "livekit,gemini"

# OpenAI WebRTC (cmake)
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo run -p adk-realtime --example openai_webrtc --features openai-webrtc

# Video avatars
cargo run -p adk-realtime --example heygen_avatar --features heygen-avatar
cargo run -p adk-realtime --example did_avatar --features did-avatar
```

## Testing

```bash
cargo nextest run -p adk-realtime
cargo nextest run -p adk-realtime --features full
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo nextest run -p adk-realtime --features openai-webrtc

# Integration tests that need credentials are #[ignore]
cargo test -p adk-realtime --features vertex-live -- --ignored
```

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://github.com/zavora-ai/adk-rust) framework for building AI agents in Rust.
