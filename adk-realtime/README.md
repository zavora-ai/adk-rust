# adk-realtime

Real-time bidirectional audio streaming for Rust Agent Development Kit (ADK-Rust) agents.

[![Crates.io](https://img.shields.io/crates/v/adk-realtime.svg)](https://crates.io/crates/adk-realtime)
[![Documentation](https://docs.rs/adk-realtime/badge.svg)](https://docs.rs/adk-realtime)
[![License](https://img.shields.io/crates/l/adk-realtime.svg)](LICENSE)

## Overview

`adk-realtime` provides a unified interface for building voice-enabled AI agents using real-time streaming APIs. It follows the **OpenAI Agents SDK pattern** with a separate, decoupled implementation that integrates seamlessly with the ADK agent ecosystem.

## Features

- **RealtimeAgent** — Implements `adk_core::Agent` with full callback/tool/instruction support
- **Multiple Providers** — OpenAI Realtime API, Gemini Live API, Vertex AI Live API
- **Multiple Transports** — WebSocket, WebRTC (OpenAI), LiveKit bridge
- **Audio Streaming** — Bidirectional audio with PCM16, G711, Opus formats
- **Voice Activity Detection** — Server-side VAD for natural conversation flow
- **Tool Calling** — Real-time function/tool execution during voice conversations
- **Agent Handoff** — Transfer between agents using `sub_agents`
- **Feature Flags** — Pay only for what you use; all transports are opt-in

## Architecture

```
              ┌─────────────────────────────────────────┐
              │              Agent Trait                 │
              │  (name, description, run, sub_agents)    │
              └────────────────┬────────────────────────┘
                               │
       ┌───────────────────────┼───────────────────────┐
       │                       │                       │
┌──────▼──────┐      ┌─────────▼─────────┐   ┌─────────▼─────────┐
│  LlmAgent   │      │  RealtimeAgent    │   │  SequentialAgent  │
│ (text-based) │      │  (voice-based)    │   │   (workflow)      │
└─────────────┘      └───────────────────┘   └───────────────────┘
```

### Transport Layer

```
┌──────────────────────────────────────────────────────────────┐
│                    RealtimeSession trait                      │
├──────────────┬──────────────┬──────────────┬─────────────────┤
│ OpenAI WS    │ OpenAI WebRTC│ Gemini Live  │ Vertex AI Live  │
│ (openai)     │ (openai-     │ (gemini)     │ (vertex-live)   │
│              │  webrtc)     │              │                 │
└──────────────┴──────────────┴──────────────┴─────────────────┘

┌──────────────────────────────────────────────────────────────┐
│              LiveKit WebRTC Bridge (livekit)                  │
│  LiveKitEventHandler · bridge_input · bridge_gemini_input    │
└──────────────────────────────────────────────────────────────┘
```

## Supported Providers & Transports

| Provider | Model | Transport | Feature Flag | Description |
|----------|-------|-----------|--------------|-------------|
| OpenAI | `gpt-4o-realtime-preview-2024-12-17` | WebSocket | `openai` | Stable realtime model |
| OpenAI | `gpt-realtime` | WebSocket | `openai` | Latest model with improved speech & function calling |
| OpenAI | `gpt-4o-realtime-*` | WebRTC | `openai-webrtc` | Browser-grade transport with Opus codec |
| Google | `gemini-live-2.5-flash-native-audio` | WebSocket | `gemini` | Gemini Live API |
| Google | Gemini via Vertex AI | WebSocket + OAuth2 | `vertex-live` | Vertex AI Live with ADC authentication |
| LiveKit | Any (bridge) | WebRTC | `livekit` | Production WebRTC bridge to Gemini/OpenAI |

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
adk-realtime = { version = "0.3", features = ["openai"] }
```

### Using RealtimeAgent (Recommended)

```rust
use adk_realtime::{RealtimeAgent, openai::OpenAIRealtimeModel};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("OPENAI_API_KEY")?;
    let model = Arc::new(OpenAIRealtimeModel::new(&api_key, "gpt-4o-realtime-preview-2024-12-17"));

    let agent = RealtimeAgent::builder("voice_assistant")
        .model(model)
        .instruction("You are a helpful voice assistant.")
        .voice("alloy")
        .server_vad()
        .build()?;

    // RealtimeAgent implements the Agent trait — use with ADK runner
    Ok(())
}
```

### Using Low-Level Session API

```rust
use adk_realtime::{RealtimeModel, RealtimeConfig, ServerEvent};
use adk_realtime::openai::OpenAIRealtimeModel;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = OpenAIRealtimeModel::new(
        std::env::var("OPENAI_API_KEY")?,
        "gpt-4o-realtime-preview-2024-12-17",
    );

    let config = RealtimeConfig::default()
        .with_instruction("You are a helpful voice assistant.")
        .with_voice("alloy");

    let session = model.connect(config).await?;
    session.send_text("Hello!").await?;
    session.create_response().await?;

    while let Some(event) = session.next_event().await {
        match event? {
            ServerEvent::AudioDelta { delta, .. } => { /* play audio */ }
            ServerEvent::TextDelta { delta, .. } => print!("{}", delta),
            _ => {}
        }
    }
    Ok(())
}
```

## Transport Guides

### Vertex AI Live

Connect to Gemini Live API via Vertex AI with Application Default Credentials:

```toml
adk-realtime = { version = "0.3", features = ["vertex-live"] }
```

```rust
use adk_realtime::gemini::{GeminiLiveBackend, GeminiRealtimeModel};

// Convenience constructor — auto-discovers ADC credentials
let backend = GeminiLiveBackend::vertex_adc("my-project", "us-central1")?;

// Or manual credentials construction
let credentials = google_cloud_auth::credentials::Credentials::default().await?;
let backend = GeminiLiveBackend::Vertex {
    credentials,
    region: "us-central1".into(),
    project_id: std::env::var("GOOGLE_CLOUD_PROJECT")?,
};

let model = GeminiRealtimeModel::new(backend, "models/gemini-live-2.5-flash-native-audio");
let session = model.connect(config).await?;
```

Prerequisites:
- Google Cloud project with Vertex AI API enabled
- ADC configured (`gcloud auth application-default login`)

### OpenAI WebRTC

Lower-latency audio transport using Sans-IO WebRTC with Opus codec:

```toml
adk-realtime = { version = "0.3", features = ["openai-webrtc"] }
```

```rust
use adk_realtime::openai::{OpenAIRealtimeModel, OpenAITransport};

let model = OpenAIRealtimeModel::new(api_key, "gpt-4o-realtime-preview-2024-12-17")
    .with_transport(OpenAITransport::WebRTC);
let session = model.connect(config).await?;
```

Build requirement: `cmake` must be installed (the `audiopus` crate builds the Opus C library from source). With cmake >= 4.0, set the environment variable:

```bash
export CMAKE_POLICY_VERSION_MINIMUM=3.5
```

### LiveKit WebRTC Bridge

Bridge any `EventHandler` to a LiveKit room for production voice apps:

```toml
adk-realtime = { version = "0.3", features = ["livekit", "openai"] }
```

```rust
use adk_realtime::livekit::{LiveKitEventHandler, bridge_input};

// Wrap your event handler to publish model audio to LiveKit
let lk_handler = LiveKitEventHandler::new(inner_handler, audio_source, 24000, 1);

// Bridge participant audio from LiveKit into the RealtimeRunner
tokio::spawn(bridge_input(remote_track, runner));
```

For Gemini's 16 kHz format, use `bridge_gemini_input` instead.

## Feature Flags

| Flag | Dependencies | Description |
|------|-------------|-------------|
| `openai` | `async-openai`, `tokio-tungstenite` | OpenAI Realtime API (WebSocket) |
| `gemini` | `tokio-tungstenite`, `adk-gemini` | Gemini Live API (AI Studio) |
| `vertex-live` | `gemini` + `google-cloud-auth` | Vertex AI Live API (OAuth2/ADC) |
| `livekit` | `livekit`, `livekit-api` | LiveKit WebRTC bridge |
| `openai-webrtc` | `openai` + `str0m`, `audiopus`, `reqwest` | OpenAI WebRTC transport (requires cmake) |
| `full` | all of the above except openai-webrtc | Everything that doesn't require cmake |
| `full-webrtc` | `full` + `openai-webrtc` | Everything including WebRTC (requires cmake) |

Default features: none. You opt in to exactly what you need.


### Feature Flag Dependency Graph

```
vertex-live  ──► gemini + google-cloud-auth
openai-webrtc ──► openai + str0m + audiopus + reqwest
livekit      ──► livekit + livekit-api
full         ──► openai + gemini + vertex-live + livekit
full-webrtc  ──► full + openai-webrtc
```

## RealtimeAgent Features

### Shared with LlmAgent

| Feature | Description |
|---------|-------------|
| `instruction(str)` | Static system instruction |
| `instruction_provider(fn)` | Dynamic instruction based on context |
| `global_instruction(str)` | Global instruction (prepended) |
| `tool(Arc<dyn Tool>)` | Register a tool |
| `sub_agent(Arc<dyn Agent>)` | Register sub-agent for handoffs |
| `before_agent_callback` | Called before agent runs |
| `after_agent_callback` | Called after agent completes |
| `before_tool_callback` | Called before tool execution |
| `after_tool_callback` | Called after tool execution |

### Realtime-Specific

| Feature | Description |
|---------|-------------|
| `voice(str)` | Voice selection ("alloy", "coral", "sage", etc.) |
| `server_vad()` | Enable server-side VAD with defaults |
| `vad(VadConfig)` | Custom VAD configuration |
| `modalities(vec)` | Output modalities (["text", "audio"]) |
| `on_audio(callback)` | Callback for audio output events |
| `on_transcript(callback)` | Callback for transcript events |
| `on_speech_started(callback)` | Callback when speech detected |
| `on_speech_stopped(callback)` | Callback when speech ends |

## Event Types

### Server Events

| Event | Description |
|-------|-------------|
| `SessionCreated` | Connection established |
| `AudioDelta` | Audio chunk (base64 PCM or Opus) |
| `TextDelta` | Text response chunk |
| `TranscriptDelta` | Input audio transcript |
| `FunctionCallDone` | Tool call request |
| `ResponseDone` | Response completed |
| `SpeechStarted` | VAD detected speech |
| `SpeechStopped` | VAD detected silence |
| `Error` | Error occurred |

### Client Events

| Event | Description |
|-------|-------------|
| `AudioAppend` | Send audio chunk |
| `AudioCommit` | Commit audio buffer |
| `ItemCreate` | Send text or tool response |
| `ResponseCreate` | Request a response |
| `ResponseCancel` | Interrupt response |
| `SessionUpdate` | Update configuration |

## Audio Formats

| Format | Sample Rate | Bits | Channels | Provider |
|--------|-------------|------|----------|----------|
| PCM16 | 24000 Hz | 16 | Mono | OpenAI |
| PCM16 | 16000 Hz | 16 | Mono | Gemini (input) |
| PCM16 | 24000 Hz | 16 | Mono | Gemini (output) |
| Opus | 24000 Hz | — | Mono | OpenAI WebRTC |
| G711 u-law | 8000 Hz | 8 | Mono | OpenAI |
| G711 A-law | 8000 Hz | 8 | Mono | OpenAI |

## Error Types

Transport-specific error variants with actionable context:

| Variant | Feature | Description |
|---------|---------|-------------|
| `OpusCodecError` | `openai-webrtc` | Opus encoding/decoding failures |
| `WebRTCError` | `openai-webrtc` | WebRTC connection and signaling failures |
| `LiveKitError` | `livekit` | LiveKit bridge failures |
| `AuthError` | `vertex-live` | OAuth2/ADC credential failures |
| `ConfigError` | all | Missing or invalid configuration |
| `ConnectionError` | all | Transport connection failures |

## Examples

```bash
# Vertex AI Live voice assistant (requires ADC + GCP project)
cargo run --example vertex_live_voice --features vertex-live

# LiveKit bridge with OpenAI model (requires LiveKit server)
cargo run --example livekit_openai --features "livekit,openai"

# LiveKit bridge with Gemini model (requires LiveKit server)
cargo run --example livekit_gemini --features "livekit,gemini"

# OpenAI WebRTC low-latency session (requires cmake + API key)
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo run --example openai_webrtc --features openai-webrtc
```

## Testing

```bash
# Property tests (no credentials needed)
cargo test -p adk-realtime --test error_context_tests
cargo test -p adk-realtime --features vertex-live --test vertex_url_property_tests
cargo test -p adk-realtime --features livekit --test livekit_delegation_tests
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo test -p adk-realtime --features openai-webrtc --test opus_roundtrip_tests
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo test -p adk-realtime --features openai-webrtc --test sdp_offer_tests

# All features
CMAKE_POLICY_VERSION_MINIMUM=3.5 cargo test -p adk-realtime --features full

# Integration tests (require real credentials, marked #[ignore])
cargo test -p adk-realtime --features vertex-live -- --ignored
```

## Compilation Verification

```bash
cargo check -p adk-realtime                          # default (no deps)
cargo check -p adk-realtime --features openai        # OpenAI WebSocket
cargo check -p adk-realtime --features gemini        # Gemini Live
cargo check -p adk-realtime --features vertex-live   # Vertex AI Live
cargo check -p adk-realtime --features livekit       # LiveKit bridge
CMAKE_POLICY_VERSION_MINIMUM=3.5 \
  cargo check -p adk-realtime --features openai-webrtc  # OpenAI WebRTC
CMAKE_POLICY_VERSION_MINIMUM=3.5 \
  cargo check -p adk-realtime --features full            # everything
```

## Feature Flags

| Flag | Description | Requires |
|------|-------------|----------|
| `openai` | OpenAI Realtime API (WebSocket) | |
| `gemini` | Gemini Live API (WebSocket) | |
| `vertex-live` | Vertex AI Live API (OAuth2 via ADC) | GCP credentials |
| `livekit` | LiveKit WebRTC bridge | LiveKit server |
| `openai-webrtc` | OpenAI WebRTC transport with Opus codec | cmake |
| `full` | All providers except WebRTC (no cmake needed) | |
| `full-webrtc` | Everything including WebRTC | cmake |

### Vertex AI Live

Connect to Gemini via Vertex AI with Application Default Credentials:

```rust
use adk_realtime::gemini::{GeminiLiveBackend, GeminiLiveModel, build_vertex_live_url};

// Uses ADC — no API key needed, just `gcloud auth application-default login`
let model = GeminiLiveModel::new(GeminiLiveBackend::Vertex {
    project_id: "my-project".into(),
    region: "us-central1".into(),
    model: "gemini-live-2.5-flash-native-audio".into(),
});
```

### Feature Flag Graph

```
vertex-live  → gemini + google-cloud-auth
livekit      → livekit + livekit-api
openai-webrtc → openai + str0m + audiopus (requires cmake)
full         → openai + gemini + vertex-live + livekit
full-webrtc  → full + openai-webrtc
```
```

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://github.com/zavora-ai/adk-rust) framework for building AI agents in Rust.
