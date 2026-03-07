# ADK-Rust Real-Time Streaming Architecture

## Overview

This document outlines the architecture for adding real-time bidirectional audio/video streaming to ADK-Rust, enabling natural voice conversations with AI agents.

## Research Summary

### Gemini Live API
- **Protocol**: WebSocket bidirectional streaming
- **Audio Input**: 16-bit PCM, 16kHz, mono
- **Audio Output**: 24kHz sample rate
- **Features**: VAD, interruptions, function calling, video input
- **Model**: `gemini-2.5-flash-native-audio-preview-12-2025`
- **Auth**: Ephemeral tokens for client-side, API key for server-side

### OpenAI Realtime API
- **Protocol**: WebSocket
- **Audio Format**: PCM16, 24kHz sample rate
- **Features**: VAD (server_vad mode), interruptions, function calling
- **Model**: `gpt-4o-realtime-preview-2024-12-17`
- **Auth**: API key (relay server recommended for browser)

### ADK Python Reference
- Uses `run_live()` method for real-time interactions
- `LiveRequestQueue` for message management
- Supports text, audio, and video inputs
- Streaming tool results back to agents

---

## Proposed Architecture

### Option A: Extend `adk-model` (Recommended)

Add real-time capabilities directly to `adk-model` with a new trait:

```
adk-model/
├── src/
│   ├── lib.rs
│   ├── model.rs          # Existing Model trait
│   ├── realtime.rs       # NEW: RealtimeModel trait
│   ├── gemini/
│   │   ├── client.rs     # Existing
│   │   └── live.rs       # NEW: Gemini Live implementation
│   └── openai/
│       ├── client.rs     # Existing
│       └── realtime.rs   # NEW: OpenAI Realtime implementation
```

**Pros**:
- Unified model abstraction
- Shares auth, config patterns
- Simpler dependency graph

**Cons**:
- Larger crate size
- WebSocket dependencies in model crate

### Option B: New `adk-realtime` Crate

Separate crate for real-time functionality:

```
adk-realtime/
├── Cargo.toml
├── src/
│   ├── lib.rs
│   ├── session.rs        # RealtimeSession trait
│   ├── events.rs         # Event types
│   ├── audio.rs          # Audio codec utilities
│   ├── gemini/
│   │   └── live.rs       # Gemini Live client
│   └── openai/
│       └── realtime.rs   # OpenAI Realtime client
```

**Pros**:
- Optional dependency
- Clean separation of concerns
- Smaller core crates

**Cons**:
- Another crate to maintain
- Potential code duplication

### Recommendation: **Option A** with feature flags

```toml
[dependencies]
adk-model = { version = "0.3", features = ["realtime"] }
```

---

## Core Traits Design

### RealtimeSession Trait

```rust
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

/// Audio format specification
#[derive(Clone, Debug)]
pub struct AudioFormat {
    pub sample_rate: u32,      // e.g., 16000, 24000
    pub channels: u8,          // 1 = mono, 2 = stereo
    pub bits_per_sample: u8,   // 16 for PCM16
    pub encoding: AudioEncoding,
}

#[derive(Clone, Debug)]
pub enum AudioEncoding {
    Pcm16,
    Opus,
    Mp3,
}

/// Events sent from client to server
#[derive(Clone, Debug)]
pub enum ClientEvent {
    /// Audio chunk from microphone
    AudioInput { data: Vec<u8> },

    /// Text input (typed message)
    TextInput { text: String },

    /// Video frame (optional)
    VideoInput { data: Vec<u8>, mime_type: String },

    /// Tool/function response
    ToolResponse { call_id: String, result: serde_json::Value },

    /// User interrupted the agent
    Interrupt,

    /// End the session
    Close,
}

/// Events received from server
#[derive(Clone, Debug)]
pub enum ServerEvent {
    /// Session established
    SessionCreated { session_id: String },

    /// Audio chunk to play
    AudioOutput { data: Vec<u8> },

    /// Text output (transcript or response)
    TextOutput { text: String, is_final: bool },

    /// Agent wants to call a tool
    ToolCall { call_id: String, name: String, args: serde_json::Value },

    /// Agent turn started
    TurnStarted,

    /// Agent turn completed
    TurnCompleted,

    /// User speech detected (VAD)
    UserSpeechStarted,

    /// User stopped speaking
    UserSpeechEnded,

    /// Error occurred
    Error { code: String, message: String },

    /// Session ended
    SessionClosed,
}

/// Configuration for real-time session
#[derive(Clone, Debug)]
pub struct RealtimeConfig {
    /// System instruction for the agent
    pub instruction: Option<String>,

    /// Voice to use for audio output
    pub voice: Option<String>,

    /// Enable voice activity detection
    pub vad_enabled: bool,

    /// Audio input format
    pub input_format: AudioFormat,

    /// Audio output format
    pub output_format: AudioFormat,

    /// Available tools/functions
    pub tools: Vec<ToolDeclaration>,

    /// Model-specific options
    pub model_options: serde_json::Value,
}

/// A real-time bidirectional streaming session
#[async_trait]
pub trait RealtimeSession: Send + Sync {
    /// Send an event to the server
    async fn send(&self, event: ClientEvent) -> Result<()>;

    /// Receive events from the server
    fn events(&self) -> Pin<Box<dyn Stream<Item = Result<ServerEvent>> + Send>>;

    /// Get the session ID
    fn session_id(&self) -> &str;

    /// Check if session is connected
    fn is_connected(&self) -> bool;

    /// Close the session gracefully
    async fn close(&self) -> Result<()>;
}

/// Factory for creating real-time sessions
#[async_trait]
pub trait RealtimeModel: Send + Sync {
    /// Connect and create a new real-time session
    async fn connect(&self, config: RealtimeConfig) -> Result<Box<dyn RealtimeSession>>;

    /// Get supported audio formats
    fn supported_input_formats(&self) -> Vec<AudioFormat>;
    fn supported_output_formats(&self) -> Vec<AudioFormat>;

    /// Check if this model supports real-time
    fn supports_realtime(&self) -> bool { true }
}
```

---

## Provider Implementations

### Gemini Live

```rust
// adk-model/src/gemini/live.rs

use tokio_tungstenite::{connect_async, WebSocketStream};
use futures::{SinkExt, StreamExt};

pub struct GeminiLiveSession {
    ws: WebSocketStream<...>,
    session_id: String,
    config: RealtimeConfig,
}

impl GeminiLiveSession {
    pub async fn connect(api_key: &str, model: &str, config: RealtimeConfig) -> Result<Self> {
        // WebSocket URL format:
        // wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent

        let url = format!(
            "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent?key={}",
            api_key
        );

        let (ws, _) = connect_async(&url).await?;

        // Send setup message
        let setup = json!({
            "setup": {
                "model": format!("models/{}", model),
                "generation_config": {
                    "response_modalities": ["AUDIO"],
                    "speech_config": {
                        "voice_config": {
                            "prebuilt_voice_config": {
                                "voice_name": config.voice.unwrap_or("Puck".into())
                            }
                        }
                    }
                },
                "system_instruction": {
                    "parts": [{ "text": config.instruction.unwrap_or_default() }]
                },
                "tools": convert_tools(&config.tools)
            }
        });

        // ...
    }
}

#[async_trait]
impl RealtimeSession for GeminiLiveSession {
    async fn send(&self, event: ClientEvent) -> Result<()> {
        match event {
            ClientEvent::AudioInput { data } => {
                // Send as realtime_input with audio chunk
                let msg = json!({
                    "realtime_input": {
                        "media_chunks": [{
                            "mime_type": "audio/pcm",
                            "data": base64::encode(&data)
                        }]
                    }
                });
                self.ws.send(Message::Text(msg.to_string())).await?;
            }
            ClientEvent::TextInput { text } => {
                let msg = json!({
                    "client_content": {
                        "turns": [{
                            "role": "user",
                            "parts": [{ "text": text }]
                        }],
                        "turn_complete": true
                    }
                });
                self.ws.send(Message::Text(msg.to_string())).await?;
            }
            // ...
        }
        Ok(())
    }

    // ...
}
```

### OpenAI Realtime

```rust
// adk-model/src/openai/realtime.rs

pub struct OpenAIRealtimeSession {
    ws: WebSocketStream<...>,
    session_id: String,
}

impl OpenAIRealtimeSession {
    pub async fn connect(api_key: &str, model: &str, config: RealtimeConfig) -> Result<Self> {
        // WebSocket URL:
        // wss://api.openai.com/v1/realtime?model=gpt-4o-realtime-preview

        let url = format!(
            "wss://api.openai.com/v1/realtime?model={}",
            model
        );

        let request = Request::builder()
            .uri(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("OpenAI-Beta", "realtime=v1")
            .body(())?;

        let (ws, _) = connect_async(request).await?;

        // Send session.update
        let update = json!({
            "type": "session.update",
            "session": {
                "modalities": ["text", "audio"],
                "instructions": config.instruction,
                "voice": config.voice.unwrap_or("alloy".into()),
                "input_audio_format": "pcm16",
                "output_audio_format": "pcm16",
                "turn_detection": {
                    "type": if config.vad_enabled { "server_vad" } else { "none" }
                },
                "tools": convert_tools(&config.tools)
            }
        });

        // ...
    }
}
```

---

## Integration with ADK Agent System

### RealtimeAgent Wrapper

```rust
// adk-agent/src/realtime_agent.rs

use adk_core::{Agent, Tool};
use adk_model::{RealtimeModel, RealtimeSession, RealtimeConfig};

pub struct RealtimeAgentRunner {
    agent: Arc<dyn Agent>,
    model: Arc<dyn RealtimeModel>,
    tools: Vec<Arc<dyn Tool>>,
}

impl RealtimeAgentRunner {
    pub async fn run_live(&self, config: RealtimeConfig) -> Result<LiveSession> {
        let session = self.model.connect(config).await?;

        // Spawn event processing loop
        let tools = self.tools.clone();
        let event_stream = session.events();

        tokio::spawn(async move {
            while let Some(event) = event_stream.next().await {
                match event {
                    Ok(ServerEvent::ToolCall { call_id, name, args }) => {
                        // Find and execute tool
                        if let Some(tool) = tools.iter().find(|t| t.name() == name) {
                            let result = tool.execute(ctx, args).await;
                            session.send(ClientEvent::ToolResponse {
                                call_id,
                                result
                            }).await;
                        }
                    }
                    // Forward other events to user callback
                    _ => { /* ... */ }
                }
            }
        });

        Ok(LiveSession { session })
    }
}
```

---

## Audio Utilities

```rust
// adk-model/src/audio.rs

/// Convert between audio formats
pub fn resample(
    input: &[i16],
    input_rate: u32,
    output_rate: u32
) -> Vec<i16> {
    // Use rubato or similar for high-quality resampling
}

/// Encode PCM to Opus
pub fn encode_opus(pcm: &[i16], sample_rate: u32) -> Vec<u8> {
    // Use opus crate
}

/// Decode Opus to PCM
pub fn decode_opus(opus: &[u8]) -> Vec<i16> {
    // Use opus crate
}

/// Simple Voice Activity Detection
pub fn detect_speech(audio: &[i16], threshold: f32) -> bool {
    let energy: f32 = audio.iter()
        .map(|&s| (s as f32).powi(2))
        .sum::<f32>() / audio.len() as f32;
    energy.sqrt() > threshold
}
```

---

## WebSocket Server Support

For `adk-server`, add WebSocket endpoint:

```rust
// adk-server/src/websocket.rs

use axum::{
    extract::ws::{WebSocket, WebSocketUpgrade},
    response::Response,
};

pub async fn realtime_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> Response {
    ws.on_upgrade(|socket| handle_realtime_session(socket, state))
}

async fn handle_realtime_session(mut socket: WebSocket, state: AppState) {
    // Create realtime session with the model
    let config = RealtimeConfig::default();
    let session = state.realtime_model.connect(config).await.unwrap();

    // Bidirectional relay
    loop {
        tokio::select! {
            // Client -> Model
            Some(msg) = socket.recv() => {
                match msg {
                    Ok(Message::Binary(data)) => {
                        session.send(ClientEvent::AudioInput { data }).await;
                    }
                    Ok(Message::Text(text)) => {
                        let event: ClientEvent = serde_json::from_str(&text)?;
                        session.send(event).await;
                    }
                    _ => break,
                }
            }
            // Model -> Client
            Some(event) = session.events().next() => {
                match event {
                    Ok(ServerEvent::AudioOutput { data }) => {
                        socket.send(Message::Binary(data)).await;
                    }
                    Ok(event) => {
                        let json = serde_json::to_string(&event)?;
                        socket.send(Message::Text(json)).await;
                    }
                    Err(e) => break,
                }
            }
        }
    }
}
```

---

## Dependencies

Add to `adk-model/Cargo.toml`:

```toml
[dependencies]
# Existing...

# Real-time (optional)
tokio-tungstenite = { version = "0.21", optional = true }
base64 = { version = "0.21", optional = true }

[features]
default = []
realtime = ["tokio-tungstenite", "base64"]
gemini-live = ["realtime"]
openai-realtime = ["realtime"]
```

---

## Implementation Phases

### Phase 1: Core Infrastructure
- [ ] Define `RealtimeSession` and `RealtimeModel` traits
- [ ] Add WebSocket utilities
- [ ] Audio format conversion helpers

### Phase 2: Gemini Live
- [ ] Implement `GeminiLiveSession`
- [ ] Handle setup, audio streaming, tool calls
- [ ] Test with basic voice interaction

### Phase 3: OpenAI Realtime
- [ ] Implement `OpenAIRealtimeSession`
- [ ] Handle session events, VAD, interruptions
- [ ] Test with function calling

### Phase 4: Agent Integration
- [ ] `RealtimeAgentRunner` for tool execution
- [ ] Integration with existing Agent trait
- [ ] Callback system for events

### Phase 5: Server Support
- [ ] WebSocket endpoint in `adk-server`
- [ ] Client SDK helpers
- [ ] Web demo example

---

## Example Usage

```rust
use adk_model::gemini::{GeminiModel, GeminiLiveModel};
use adk_model::{RealtimeModel, RealtimeConfig, ClientEvent, ServerEvent};
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;

    // Create realtime model
    let model = GeminiLiveModel::new(&api_key, "gemini-live-2.5-flash-native-audio")?;

    // Configure session
    let config = RealtimeConfig {
        instruction: Some("You are a helpful voice assistant.".into()),
        voice: Some("Kore".into()),
        vad_enabled: true,
        ..Default::default()
    };

    // Connect
    let session = model.connect(config).await?;
    println!("Connected: {}", session.session_id());

    // Handle events
    let events = session.events();
    tokio::spawn(async move {
        while let Some(event) = events.next().await {
            match event {
                Ok(ServerEvent::AudioOutput { data }) => {
                    // Play audio through speakers
                    play_audio(&data);
                }
                Ok(ServerEvent::TextOutput { text, is_final }) => {
                    if is_final {
                        println!("Agent: {}", text);
                    }
                }
                Ok(ServerEvent::TurnCompleted) => {
                    println!("--- Agent finished speaking ---");
                }
                _ => {}
            }
        }
    });

    // Stream audio from microphone
    let mic = Microphone::new(16000, 1)?;
    loop {
        let chunk = mic.read_chunk()?;
        session.send(ClientEvent::AudioInput { data: chunk }).await?;
    }
}
```

---

## References

- [Gemini Live API Docs](https://ai.google.dev/gemini-api/docs/live)
- [OpenAI Realtime API](https://platform.openai.com/docs/guides/realtime)
- [ADK Python Streaming](https://google.github.io/adk-docs/streaming/)
- [tokio-tungstenite](https://docs.rs/tokio-tungstenite)
