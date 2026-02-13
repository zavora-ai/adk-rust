# adk-realtime

Real-time bidirectional audio streaming for Rust Agent Development Kit (ADK-Rust) agents.

## Overview

`adk-realtime` provides a unified interface for building voice-enabled AI agents using real-time streaming APIs from various providers. It follows the **OpenAI Agents SDK pattern** with a separate, decoupled implementation that integrates seamlessly with the ADK agent ecosystem.

## Features

- **RealtimeAgent**: Implemented `adk_core::Agent` with full callback/tool/instruction support
- **Multiple Providers**: Unified interface for OpenAI Realtime and Gemini Live API
- **LiveKit Integration**: Direct bridge for using AI agents in LiveKit real-time audio rooms
- **Audio Streaming**: Bidirectional audio with PCM16, G711, and other formats
- **Voice Activity Detection**: Server-side VAD for natural conversation flow
- **Tool Calling**: Real-time function/tool execution during voice conversations
- **Agent Handoff**: Transfer between agents using `sub_agents`

## Architecture

```
              ┌─────────────────────────────────────────┐
              │              Agent Trait                │
              │  (name, description, run, sub_agents)   │
              └────────────────┬────────────────────────┘
                               │
       ┌───────────────────────┼───────────────────────┐
       │                       │                       │
┌──────▼──────┐      ┌─────────▼─────────┐   ┌─────────▼─────────┐
│  LlmAgent   │      │  RealtimeAgent    │   │  SequentialAgent  │
│ (text-based)│      │  (voice-based)    │   │   (workflow)      │
└─────────────┘      └───────────────────┘   └───────────────────┘
```

`RealtimeAgent` shares the same features as `LlmAgent`:
- Static and dynamic instructions (`instruction`, `instruction_provider`)
- Tool registration and execution
- Callbacks (`before_agent`, `after_agent`, `before_tool`, `after_tool`)
- Sub-agent handoffs via `transfer_to_agent`

## Supported Providers

| Provider | Model | Feature Flag | Description |
|----------|-------|--------------|-------------|
| OpenAI | `gpt-4o-realtime-preview-2024-12-17` | `openai` | Stable realtime model |
| OpenAI | `gpt-realtime` | `openai` | Latest model with improved speech & function calling |
| Google | `gemini-live-2.5-flash-native-audio` | `gemini` | Gemini Live API (Latest) |

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
adk-realtime = { version = "0.3.0", features = ["openai"] }
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
        .server_vad()  // Enable server-side voice activity detection
        .build()?;

    // RealtimeAgent implements the Agent trait
    // Use with ADK runner or directly via agent.run(ctx)
    Ok(())
}
```

### Using Gemini Live

```rust
use adk_realtime::{RealtimeAgent, gemini::GeminiRealtimeModel};
use adk_gemini::GeminiLiveBackend;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Public API (API Key)
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let backend = GeminiLiveBackend::Public { api_key };
    
    // 2. Vertex AI (OAuth)
    // let backend = GeminiLiveBackend::Vertex(vertex_context);

    let model = Arc::new(GeminiRealtimeModel::new(
        backend, 
        "models/gemini-live-2.5-flash-native-audio"
    ));

    let agent = RealtimeAgent::builder("gemini_voice")
        .model(model)
        .instruction("You are a helpful assistant.")
        // Gemini voice names: "Puck", "Charon", "Kore", "Fenrir", "Aoede"
        .voice("Puck") 
        .build()?;

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

    // Send text or audio
    session.send_text("Hello!").await?;
    session.create_response().await?;

    // Process events
    while let Some(event) = session.next_event().await {
        match event? {
            ServerEvent::AudioDelta { delta, .. } => {
                // Play audio (delta is 24kHz PCM bytes)
            }
            ServerEvent::TextDelta { delta, .. } => {
                print!("{}", delta);
            }
            ServerEvent::FunctionCallDone { name, arguments, call_id, .. } => {
                // Execute tool and send response
                let result = execute_tool(&name, &arguments);
                session.send_tool_response(ToolResponse {
                    call_id,
                    output: result,
                }).await?;
            }
            _ => {}
        }
    }

    Ok(())
}
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
| `on_audio(callback)` | Callback for audio output events (`Fn(&[u8], &str)`) |
| `on_transcript(callback)` | Callback for transcript events |
| `on_speech_started(callback)` | Callback when speech detected |
| `on_speech_stopped(callback)` | Callback when speech ends |

## Event Types

### Server Events

| Event | Description |
|-------|-------------|
| `SessionCreated` | Connection established |
| `AudioDelta` | Audio chunk (Vec<u8> PCM) |
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
| `InputAudioBufferAppend` | Send audio (via `bytes::Bytes`) |

## Audio Transport Architecture

### Zero-Copy Performance
The entire `adk-realtime` framework uses `bytes::Bytes` for audio transport. This ensures that large audio buffers are shared rather than copied as they flow from the AI provider through the agent loop to the transport layers (e.g., LiveKit or WebRTC).

### OpenAI WebRTC (Sans-IO)
The OpenAI WebRTC implementation utilizes the `rtc` crate (v0.8.5) and follows a **Sans-IO** pattern for maximum reliability and control. It is engineered for **production stability**:

- **Stable SSRC Latching**: Locks onto a single SSRC at startup to prevent decoder resets and audio glitches on the server side.
- **Zero-Allocation Hot Loop**: Uses pre-allocated byte buffers for the critical 20ms audio path, eliminating heap allocation jitter.
- **Bounded Message Queues**: Enforces strict capacity limits (50 items) on pending DataChannel messages to prevent memory leaks during network interruptions.
- **Robust Connection Monitoring**: Immediate termination and error reporting on `RTCPeerConnectionState::Failed`, enabling rapid application-level recovery.
- **RFC 7587 Compliance**: Strictly follows the Opus RTP clock rate (48kHz) regardless of input sample rate.
- **DataChannel Integration**: Securely routes JSON control events (`oai-events`) alongside the high-priority audio media track.

### Gemini Live Optimization
The Gemini Live implementation is optimized for the `gemini-live-2.5-flash-native-audio` model:
- **Native Audio Support**: Direct streaming of 24kHz PCM16 bytes.
- **Fast Turnaround**: Leveraging the model's native SAD (Server-side Activity Detection) for sub-second conversational latency.


| Format | Sample Rate | Bits | Channels | Provider |
|--------|-------------|------|----------|----------|
| PCM16 | 24000 Hz | 16 | Mono | OpenAI |
| PCM16 | 16000 Hz | 16 | Mono | Gemini (input) |
| PCM16 | 24000 Hz | 16 | Mono | Gemini (output) |
| G711 u-law | 8000 Hz | 8 | Mono | OpenAI |
| G711 A-law | 8000 Hz | 8 | Mono | OpenAI |

## Voice Activity Detection

### Server VAD (Recommended)

```rust
let agent = RealtimeAgent::builder("assistant")
    .model(model)
    .server_vad()  // Uses default settings
    .build()?;
```

### Custom VAD

```rust
use adk_realtime::{VadConfig, VadMode};

let agent = RealtimeAgent::builder("assistant")
    .model(model)
    .vad(VadConfig {
        mode: VadMode::ServerVad,
        threshold: Some(0.5),
        prefix_padding_ms: Some(300),
        silence_duration_ms: Some(500),
        interrupt_response: Some(true),
        eagerness: None,
    })
    .build()?;
```

## Agent Handoffs

```rust
let booking_agent = Arc::new(/* ... */);
let support_agent = Arc::new(/* ... */);

let agent = RealtimeAgent::builder("receptionist")
    .model(model)
    .instruction("You are a receptionist. Transfer to booking_agent for reservations.")
    .sub_agent(booking_agent)
    .sub_agent(support_agent)
    .build()?;

// Agent can now call transfer_to_agent("booking_agent") during conversation
```

## Examples

Run the included examples to see realtime agents in action:

```bash
# Basic text-only realtime session
cargo run --example realtime_basic --features realtime-openai

# Voice assistant with server-side VAD
cargo run --example realtime_vad --features realtime-openai

# Tool calling during voice conversations
cargo run --example realtime_tools --features realtime-openai

# Multi-agent handoffs (receptionist routing to specialists)
cargo run --example realtime_handoff --features realtime-openai
```

### Example Descriptions

| Example | Description |
|---------|-------------|
| `realtime_basic` | Simple text-based realtime session demonstrating connection and streaming |
| `realtime_vad` | Voice assistant with Voice Activity Detection for natural conversations |
| `realtime_tools` | Real-time tool calling (weather lookup) during conversations |
| `realtime_handoff` | Multi-agent system with receptionist routing to booking, support, and sales agents |

## LiveKit Integration

The `adk-realtime` crate includes first-class support for **LiveKit**, allowing you to bridge your AI agents into real-time audio rooms.

### The Bridge Pattern

We use a "Facade" architecture where the LiveKit transport is completely decoupled from the AI provider. This allows you to "plug" any `RealtimeModel` (Gemini or OpenAI) into a LiveKit room without changing your integration logic.

- **Hearing**: `bridge_input` subscribes to a LiveKit `RemoteAudioTrack` and feeds it to the AI.
- **Speaking**: `LiveKitEventHandler` receives AI audio and pushes it to a LiveKit `NativeAudioSource`.

### Usage Example

```rust
use adk_realtime::RealtimeRunner;
use adk_realtime::livekit::{LiveKitEventHandler, bridge_input};
use livekit::native::audio_source::NativeAudioSource;

// 1. Setup LiveKit source for the AI's voice
let source = NativeAudioSource::new(AudioSourceOptions::default());

// 2. Setup your handler (can wrap any custom logic)
let lk_handler = Arc::new(LiveKitEventHandler::new(source, inner_handler));

// 3. Connect to ANY AI provider (Gemini or OpenAI)
let model = Arc::new(GeminiRealtimeModel::new(backend, "models/gemini-live-2.5-flash-native-audio"));
// OR: let model = Arc::new(OpenAIRealtimeModel::new(api_key, "gpt-4o-realtime"));

// 4. Build the runner
let runner = RealtimeRunner::builder()
    .model(model)
    .event_handler(lk_handler)
    .build()?;

// 5. Connect the room's audio to the AI
bridge_input(remote_audio_track, runner.clone());
```

### Technical Benefits

- **Backend Agnostic**: Swap between OpenAI and Gemini with a single line change.
- **Unified Audio**: Automatically handles conversion to **24kHz Mono PCM** for optimal performance.
- **Zero-Copy Intent**: Optimized for streaming raw PCM frames directly to the transport layer.

## Feature Flags

| Flag | Description |
|------|-------------|
| `openai` | Enable OpenAI Realtime API |
| `gemini` | Enable Gemini Live API |
| `livekit` | Enable LiveKit transport support |
| `full` | Enable all providers and integrations |

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
