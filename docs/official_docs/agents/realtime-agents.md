# Realtime Voice Agents

Realtime agents enable voice-based interactions with AI assistants using bidirectional audio streaming. The `adk-realtime` crate provides a unified interface for building voice-enabled agents that work with OpenAI's Realtime API and Google's Gemini Live API.

## Overview

Realtime agents differ from text-based LlmAgents in several key ways:

| Feature | LlmAgent | RealtimeAgent |
|---------|----------|---------------|
| Input | Text | Audio/Text |
| Output | Text | Audio/Text |
| Connection | HTTP requests | WebSocket |
| Latency | Request/response | Real-time streaming |
| VAD | N/A | Server-side voice detection |

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

`RealtimeAgent` implements the same `Agent` trait as `LlmAgent`, sharing:
- Instructions (static and dynamic)
- Tool registration and execution, governed by the run's tool policy, kill switch, and confirmation
- Callbacks (before_agent, after_agent, before_tool, after_tool), plus the runner's plugin callbacks
- Sub-agent handoffs

## Quick Start

### Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
adk-realtime = { version = "3.0.0", features = ["openai"] }

# For Vertex AI Live (Google Cloud with ADC auth)
# adk-realtime = { version = "3.0.0", features = ["vertex-live"] }

# For LiveKit WebRTC bridge
# adk-realtime = { version = "3.0.0", features = ["livekit"] }

# For all transports (except WebRTC which needs cmake)
# adk-realtime = { version = "3.0.0", features = ["full"] }
```

### Basic Usage

```rust
use adk_realtime::{
    RealtimeAgent, RealtimeModel, RealtimeConfig, ServerEvent,
    openai::OpenAIRealtimeModel,
};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_key = std::env::var("OPENAI_API_KEY")?;

    // Create the realtime model
    let model: Arc<dyn RealtimeModel> = Arc::new(
        OpenAIRealtimeModel::new(&api_key, "gpt-realtime-2.1")
    );

    // Build the realtime agent
    let agent = RealtimeAgent::builder("voice_assistant")
        .model(model.clone())
        .instruction("You are a helpful voice assistant. Be concise.")
        .voice("alloy")
        .server_vad()  // Enable voice activity detection
        .build()?;

    // Or use the low-level session API directly
    let config = RealtimeConfig::default()
        .with_instruction("You are a helpful assistant.")
        .with_voice("alloy")
        .with_modalities(vec!["text".to_string(), "audio".to_string()]);

    let session = model.connect(config).await?;

    // Send text and get response
    session.send_text("Hello!").await?;
    session.create_response().await?;

    // Process events
    while let Some(event) = session.next_event().await {
        match event? {
            ServerEvent::TextDelta { delta, .. } => print!("{delta}"),
            // The transcript of the spoken reply
            ServerEvent::TranscriptDelta { delta, .. } => print!("{delta}"),
            ServerEvent::AudioDelta { delta, .. } => {
                // Play audio (delta is raw PCM16 bytes)
            }
            ServerEvent::ResponseDone { .. } => break,
            _ => {}
        }
    }

    Ok(())
}
```

## Supported Providers

| Provider | Model | Transport | Feature Flag | Audio Format |
|----------|-------|-----------|--------------|--------------|
| OpenAI | `gpt-realtime-2.1` | WebSocket | `openai` | PCM16 24kHz |
| OpenAI | `gpt-realtime-2.1` | WebRTC | `openai-webrtc` | Opus |
| Google | `gemini-3.1-flash-live-preview` | WebSocket | `gemini` | PCM16 16kHz/24kHz |
| Google | Gemini via Vertex AI | WebSocket + OAuth2 | `vertex-live` | PCM16 16kHz/24kHz |
| LiveKit | Any (bridge to Gemini/OpenAI) | WebRTC | `livekit` | PCM16 |

> **Note**: `gpt-realtime-2.1` is ADK-Rust's current OpenAI realtime default.

### Transport Options

ADK-Realtime supports multiple transport layers:

- **WebSocket** (default): Direct connection to OpenAI or Gemini. Simple, low-latency, works everywhere.
- **Vertex AI Live**: Connects to Gemini via Google Cloud with OAuth2 authentication (Application Default Credentials). Use when you need enterprise auth and GCP integration.
- **LiveKit WebRTC**: Production-grade WebRTC bridge. Routes audio through a LiveKit server for scalable, multi-participant scenarios.
- **OpenAI WebRTC**: Direct WebRTC connection to OpenAI with Opus codec and data channels. Requires cmake for building the Opus C library.

## RealtimeAgent Builder

The `RealtimeAgentBuilder` provides a fluent API for configuring agents:

```rust
use adk_realtime::{RealtimeAgent, VadConfig, VadMode};
use std::sync::Arc;

let agent = RealtimeAgent::builder("assistant")
    // Required
    .model(model)

    // Instructions (same as LlmAgent)
    .instruction("You are helpful.")
    .instruction_provider(Box::new(|ctx| {
        Box::pin(async move { Ok(format!("The user's id is {}.", ctx.user_id())) })
    }))

    // Voice settings
    .voice("alloy")  // Options: alloy, coral, sage, shimmer, etc.

    // Voice Activity Detection
    .vad(VadConfig {
        mode: VadMode::ServerVad,
        threshold: Some(0.5),
        prefix_padding_ms: Some(300),
        silence_duration_ms: Some(500),
        interrupt_response: Some(true),
        eagerness: None,
    })

    // Tools (same as LlmAgent)
    .tool(Arc::new(weather_tool))
    .tool(Arc::new(search_tool))

    // Sub-agents for handoffs
    .sub_agent(booking_agent)
    .sub_agent(support_agent)

    // Callbacks: the same adk_core callback types LlmAgent takes
    .before_agent_callback(Box::new(|_ctx| Box::pin(async { Ok(None) })))
    .after_agent_callback(Box::new(|_ctx| Box::pin(async { Ok(None) })))
    .before_tool_callback(Box::new(|_ctx| Box::pin(async { Ok(None) })))
    .after_tool_callback(Box::new(|_ctx| Box::pin(async { Ok(None) })))

    // Realtime-specific callbacks: PCM audio and transcript deltas, with the item id
    .on_audio(Arc::new(|_pcm: &[u8], _item_id: &str| Box::pin(async { /* play audio */ })))
    .on_transcript(Arc::new(|_text: &str, _item_id: &str| Box::pin(async { /* show transcript */ })))

    .build()?;
```

When both are set, `instruction_provider` is used and computes the instruction for each run.
`.server_vad()` is shorthand for server VAD with its defaults.

`modalities(vec)` sets the output modalities. Unset, the provider's default applies — audio,
with a transcript, on the OpenAI Realtime API and on Gemini Live — so a default agent runs on
both. Gemini Live accepts one response modality per session, and
`gemini-3.1-flash-live-preview` only `audio`; it refuses any other setup by closing the
socket with 1007, and the run then ends with the error `model.realtime.session_closed`
(category `InvalidInput`), which carries the close code and the provider's reason. A close with
another failure code ends the run with the same error code (category `Unavailable`); a normal
close (1000, 1001) ends the run without one.

## Voice Activity Detection (VAD)

VAD enables natural conversation flow by detecting when the user starts and stops speaking.

### Server VAD (Recommended)

```rust
let agent = RealtimeAgent::builder("assistant")
    .model(model)
    .server_vad()  // Uses sensible defaults
    .build()?;
```

### Custom VAD Configuration

```rust
use adk_realtime::{VadConfig, VadMode};

let vad = VadConfig {
    mode: VadMode::ServerVad,
    threshold: Some(0.5),           // Speech detection sensitivity (0.0-1.0)
    prefix_padding_ms: Some(300),   // Audio to include before speech
    silence_duration_ms: Some(500), // Silence before ending turn
    interrupt_response: Some(true), // Allow interrupting assistant
    eagerness: None,                // For SemanticVad mode
};

let agent = RealtimeAgent::builder("assistant")
    .model(model)
    .vad(vad)
    .build()?;
```

### Semantic VAD (OpenAI)

For OpenAI realtime models, semantic VAD decides the end of a turn from what was said.
Gemini Live has no equivalent and keeps its own automatic detection:

```rust
use adk_realtime::{VadConfig, VadMode};

let vad = VadConfig {
    mode: VadMode::SemanticVad,
    eagerness: Some("high".to_string()),  // low, medium, high
    ..Default::default()
};
```

## Tool Calling

Realtime agents support tool calling during voice conversations:

```rust
use adk_realtime::{config::ToolDefinition, ToolResponse};
use serde_json::json;

// Define tools
let tools = vec![
    ToolDefinition {
        name: "get_weather".to_string(),
        description: Some("Get weather for a location".to_string()),
        parameters: Some(json!({
            "type": "object",
            "properties": {
                "location": { "type": "string" }
            },
            "required": ["location"]
        })),
    },
];

let config = RealtimeConfig::default()
    .with_tools(tools)
    .with_instruction("Use tools to help the user.");

let session = model.connect(config).await?;

// Handle tool calls in the event loop
while let Some(event) = session.next_event().await {
    match event? {
        ServerEvent::FunctionCallDone { call_id, name, arguments, .. } => {
            // Execute the tool
            let result = execute_tool(&name, &arguments);

            // Send the response
            let response = ToolResponse::new(&call_id, result);
            session.send_tool_response(response).await?;
        }
        _ => {}
    }
}
```

## Multi-Agent Handoffs

Transfer conversations between specialized agents:

```rust
// Create sub-agents
let booking_agent = Arc::new(RealtimeAgent::builder("booking_agent")
    .model(model.clone())
    .instruction("Help with reservations.")
    .build()?);

let support_agent = Arc::new(RealtimeAgent::builder("support_agent")
    .model(model.clone())
    .instruction("Help with technical issues.")
    .build()?);

// Create main agent with sub-agents
let receptionist = RealtimeAgent::builder("receptionist")
    .model(model)
    .instruction(
        "Route customers: bookings → booking_agent, issues → support_agent. \
         Use transfer_to_agent tool to hand off."
    )
    .sub_agent(booking_agent)
    .sub_agent(support_agent)
    .build()?;
```

When the model calls `transfer_to_agent`, the `RealtimeAgent` closes its session and emits an
event carrying `actions.transfer_to_agent`; the `Runner` running the agent hands control to the
target, under the same [transfer restrictions](multi-agent.md#transfer-restrictions) as any
agent. A `transfer_to_agent` call without a readable `agent_name` is answered with an error
instead of transferring.

## Governed Tool Calls

`RealtimeAgent`, `RealtimeRunner` and `IntegratedRealtimeRunner` dispatch every tool call
through `adk_core::authorize_tool_call`, the path `LlmAgent` uses:

| Control | `RealtimeAgent` | `RealtimeRunner` / `IntegratedRealtimeRunner` |
|---------|-----------------|-----------------------------------------------|
| Kill switch, `ToolPolicy`, confirmation | Read from the run's `RunConfig` | `RealtimeRunner`: `tool_governance(run_config, approval_scope)` on the builder. `IntegratedRealtimeRunner`: `tool_governance(run_config)`, with the approval scope from `identity(app, user, session)` |
| Runner plugin agent and tool callbacks | Run ahead of the agent's own | — |
| Per-call timeout | `tool_timeout` (default five minutes) | `tool_timeout` (default five minutes) |
| Result screening | `tool_result_screen` | `tool_result_screen` |

- **Malformed arguments**: a call whose arguments are not valid JSON is answered with an error,
  and the tool does not run.
- **Result screening**: a `ToolResultScreen`, such as `adk-guardrail`'s `ToolGuardrailSet`,
  rewrites or withholds each result after the after-tool stages; a screen that cannot decide
  withholds the result.
- **Error text**: a failed call is answered with the error's component, category, and code;
  `expose_tool_error_messages(true)` sends the full message.

## Audio Formats

| Format | Sample Rate | Bits | Channels | Use Case |
|--------|-------------|------|----------|----------|
| PCM16 | 24000 Hz | 16 | Mono | OpenAI (default) |
| PCM16 | 16000 Hz | 16 | Mono | Gemini input |
| G711 u-law | 8000 Hz | 8 | Mono | Telephony |
| G711 A-law | 8000 Hz | 8 | Mono | Telephony |

```rust
use adk_realtime::AudioFormat;
use adk_realtime::audio::AudioChunk;

// Create audio format
let format = AudioFormat::pcm16_24khz();

// Work with audio chunks
let chunk = AudioChunk::new(audio_bytes, format.clone());
let base64 = chunk.to_base64();
let decoded = AudioChunk::from_base64(&base64, format)?;
```

## Event Types

### Server Events

| Event | Description |
|-------|-------------|
| `SessionCreated` | Connection established |
| `AudioDelta` | Audio chunk (raw PCM16 bytes) |
| `TextDelta` | Text response chunk |
| `TranscriptDelta` | Transcript of the response audio |
| `InputTranscriptDelta` | Transcript of the user's input audio |
| `FunctionCallDone` | Tool call request |
| `ResponseDone` | Response completed |
| `SpeechStarted` | VAD detected speech start, the barge-in signal (OpenAI `speech_started`, Gemini Live `voiceActivity` `ACTIVITY_START`, or `serverContent.interrupted` when no `ACTIVITY_START` reported the speech) |
| `SpeechStopped` | VAD detected speech end (OpenAI `speech_stopped`, Gemini Live `voiceActivity` `ACTIVITY_END`) |
| `Error` | Error occurred |

### Session history

Under `Runner`, the session holds both sides of the conversation. The run's text message is
saved by the runner. With `transcription()` on the builder, which asks the provider to
transcribe the user (and, on Gemini Live, the model), each of the user's spoken turns is saved
as a user event (author `user`, `provider_metadata["adk.realtime.transcript"] = "input"`) ahead
of the model's reply:

| Provider | A spoken turn is saved |
|----------|------------------------|
| OpenAI | When its `InputTranscriptCompleted` arrives |
| Gemini Live, which streams `InputTranscriptDelta`s and never completes them | When the model's reply starts or ends |
| Either | At the end of the session, if the model has not replied |

The model's replies are saved as final text and transcript events authored by the agent, each
reply under its own event id (`{invocation}:realtime-transcript:{item}`, or
`{invocation}:realtime-text:{item}`). Gemini Live streams a reply without an item id, so the
agent names its item `reply-{uuid}` when the reply starts; a session backend that keys events
by id, such as SQLite, stores every reply of a run.

### Client Events

| Event | Description |
|-------|-------------|
| `AudioInput` | Send audio chunk |
| `AudioCommit` | Commit audio buffer |
| `ItemCreate` | Send text or tool response |
| `CreateResponse` | Request a response |
| `CancelResponse` | Cancel current response |
| `SessionUpdate` | Update configuration |

## Vertex AI Live (Google Cloud)

Connect to Gemini Live via Vertex AI with enterprise authentication (ADC, service accounts, WIF):

```rust
use adk_realtime::gemini::{GeminiLiveBackend, GeminiRealtimeModel};
use adk_realtime::{RealtimeConfig, RealtimeModel};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let project_id = std::env::var("GOOGLE_CLOUD_PROJECT")?;
    let region = std::env::var("GOOGLE_CLOUD_REGION")
        .unwrap_or_else(|_| "us-central1".to_string());

    // Use Application Default Credentials
    let backend = GeminiLiveBackend::vertex_adc(project_id, region)?;
    let model = GeminiRealtimeModel::new(backend, "models/gemini-3.1-flash-live-preview");

    let config = RealtimeConfig::default()
        .with_instruction("You are a helpful voice assistant.");

    let session = model.connect(config).await?;
    session.send_text("Hello from Vertex AI!").await?;
    session.create_response().await?;

    // Process events...
    Ok(())
}
```

`GeminiLiveBackend::vertex_adc` discovers Application Default Credentials. To supply
credentials yourself, build the variant directly with a
`google_cloud_auth::credentials::Credentials`:

```rust
use adk_realtime::gemini::{GeminiLiveBackend, GeminiRealtimeModel};

let backend = GeminiLiveBackend::Vertex {
    credentials,
    region: "us-central1".to_string(),
    project_id: "my-project-id".to_string(),
};
let model = GeminiRealtimeModel::new(backend, "models/gemini-3.1-flash-live-preview");
```

### Vertex AI Live with Tool Calling

The `vertex_live_tools` example demonstrates function calling over a Vertex AI Live session:

```rust
use adk_realtime::config::ToolDefinition;
use adk_realtime::events::ToolResponse;
use serde_json::json;

// Declare tools
let tools = vec![
    ToolDefinition {
        name: "get_weather".to_string(),
        description: Some("Get current weather for a city".to_string()),
        parameters: Some(json!({
            "type": "object",
            "properties": {
                "city": { "type": "string" }
            },
            "required": ["city"]
        })),
    },
];

let config = RealtimeConfig::default()
    .with_tools(tools)
    .with_instruction("Use tools to answer questions about weather.");

let session = model.connect(config).await?;

// Handle FunctionCallDone events and send ToolResponse back
while let Some(event) = session.next_event().await {
    match event? {
        ServerEvent::FunctionCallDone { call_id, name, arguments, .. } => {
            let result = match name.as_str() {
                "get_weather" => json!({"temperature": "22°C", "condition": "sunny"}),
                _ => json!({"error": "unknown tool"}),
            };
            session.send_tool_response(ToolResponse::new(&call_id, result)).await?;
        }
        ServerEvent::TextDelta { delta, .. } => print!("{delta}"),
        ServerEvent::ResponseDone { .. } => break,
        _ => {}
    }
}
```

### Feature Flags

| Feature | Dependencies | Use Case |
|---------|-------------|----------|
| `openai` | `async-openai` + `tokio-tungstenite` | OpenAI Realtime over WebSocket |
| `gemini` | `adk-gemini` + `tokio-tungstenite` | Gemini Live over WebSocket |
| `vertex-live` | `gemini` + `google-cloud-auth` | Vertex AI Live with ADC/service account auth |
| `livekit` | `livekit` + `livekit-api` | LiveKit WebRTC bridge |
| `openai-webrtc` | `openai` + `str0m` + `audiopus` | OpenAI WebRTC with Opus (requires cmake) |
| `full` | `openai` + `gemini` + `vertex-live` + `livekit` | All transports except WebRTC |
| `full-webrtc` | `full` + `openai-webrtc` | Everything (requires cmake) |

Through the `adk-rust` umbrella crate, `realtime` links `adk-realtime` without a transport.
Add one transport feature for each provider the application uses:

| `adk-rust` feature | Forwards to |
|--------------------|-------------|
| `openai-realtime` | `adk-realtime/openai` |
| `gemini-live` | `adk-realtime/gemini` |
| `vertex-live` | `adk-realtime/vertex-live` |
| `openai-webrtc` | `adk-realtime/openai-webrtc` (requires cmake) |
| `livekit` | `adk-realtime/livekit` |

```toml
adk-rust = { version = "3.0.0", features = ["minimal", "gemini-live"] }
```

## LiveKit WebRTC Bridge

For production voice applications, the LiveKit bridge routes audio through a LiveKit server for scalable, multi-participant scenarios.

### LiveKitConfig

Securely configure LiveKit credentials. API keys and secrets are stored using `secrecy::SecretString` and redacted in Debug output:

```rust
use adk_realtime::livekit::{LiveKitConfig, LiveKitRoomBuilder};

let config = LiveKitConfig::new(
    "wss://your-server.livekit.cloud",
    std::env::var("LIVEKIT_API_KEY")?,
    std::env::var("LIVEKIT_API_SECRET")?,
)?;
```

`LiveKitConfig::new()` validates the URL format and rejects empty credentials at construction time.

### LiveKitRoomBuilder

A typestate builder for connecting to LiveKit rooms. The `identity` field is required at compile time — `connect()` is only available after it's set:

```rust
let bundle = LiveKitRoomBuilder::new(config)
    .identity("my-agent")           // required — enables connect()
    .name("Voice Agent")            // optional display name
    .room_name("session-room-123")  // optional — auto-generated if omitted
    .auto_subscribe(true)           // subscribe to remote tracks
    .with_audio(24_000, 1)          // publish a local audio track (sample rate, channels)
    .connect()
    .await?;

// The bundle contains everything you need
let room = bundle.room;
let mut events = bundle.events;
let audio_source = bundle.audio_source;  // Some(..) when with_audio was set
let audio_track = bundle.audio_track;
```

### Bridging Audio

Use the bridge utilities to connect LiveKit audio to a `RealtimeRunner`:

```rust
use adk_realtime::livekit::{LiveKitEventHandler, bridge_input};

// Wrap your event handler to publish model audio to LiveKit; the bundle's
// `audio_source` is Some(..) because the room was built with `with_audio`
let audio_source = audio_source.expect("the room was built with with_audio");
let lk_handler = LiveKitEventHandler::new(inner_handler, audio_source, 24000, 1);

// Bridge participant audio from LiveKit into the RealtimeRunner (an Arc<RealtimeRunner>)
let input_runner = runner.clone();
tokio::spawn(async move { bridge_input(remote_track, &input_runner).await });
```

`bridge_gemini_input` does the same for a runner on a Gemini Live session.

## Examples

Run the included examples:

```bash
# OpenAI Realtime (WebSocket)
cargo run -p adk-realtime --example openai_session_update --features openai

# Vertex AI Live (requires gcloud auth application-default login)
cargo run -p adk-realtime --example vertex_live_voice --features vertex-live
cargo run -p adk-realtime --example vertex_live_tools --features vertex-live

# LiveKit Bridge (requires LiveKit server)
cargo run -p adk-realtime --example livekit_bridge --features livekit,openai
cargo run -p adk-realtime --example livekit_gemini_bridge --features livekit,gemini

# Debug utilities
cargo run -p adk-realtime --example debug_gemini --features gemini
cargo run -p adk-realtime --example debug_livekit_auth --features livekit

# OpenAI WebRTC (requires cmake)
cargo run -p adk-realtime --example openai_webrtc --features openai-webrtc
```

## Best Practices

1. **Use Server VAD**: Let the server handle speech detection for lower latency
2. **Handle interruptions**: Enable `interrupt_response` for natural conversations
3. **Keep instructions concise**: Voice responses should be brief
4. **Test with text first**: Debug your agent logic with text before adding audio
5. **Handle errors gracefully**: Network issues are common with WebSocket connections

## Comparison with OpenAI Agents SDK

ADK-Rust's realtime implementation follows the OpenAI Agents SDK pattern:

| Feature | OpenAI SDK | ADK-Rust |
|---------|------------|----------|
| Agent base class | `Agent` | `Agent` trait |
| Realtime agent | `RealtimeAgent` | `RealtimeAgent` |
| Tools | Function definitions | `Tool` trait, or `ToolDefinition` on a raw session |
| Handoffs | `transfer_to_agent` | `sub_agents` + auto-generated tool |
| Callbacks | Hooks | `before_*` / `after_*` callbacks |

---

**Previous**: [← Graph Agents](./graph-agents.md) | **Next**: [Model Providers →](../models/providers.md)
