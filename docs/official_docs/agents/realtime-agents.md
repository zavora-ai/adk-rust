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
- Tool registration and execution
- Callbacks (before_agent, after_agent, before_tool, after_tool)
- Sub-agent handoffs

## Quick Start

### Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
adk-realtime = { version = "0.3.0", features = ["openai"] }
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
        OpenAIRealtimeModel::new(&api_key, "gpt-4o-realtime-preview-2024-12-17")
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
            ServerEvent::TextDelta { delta, .. } => print!("{}", delta),
            ServerEvent::AudioDelta { delta, .. } => {
                // Play audio (delta is base64-encoded PCM)
            }
            ServerEvent::ResponseDone { .. } => break,
            _ => {}
        }
    }

    Ok(())
}
```

## Supported Providers

| Provider | Model | Feature Flag | Audio Format |
|----------|-------|--------------|--------------|
| OpenAI | `gpt-4o-realtime-preview-2024-12-17` | `openai` | PCM16 24kHz |
| OpenAI | `gpt-realtime` | `openai` | PCM16 24kHz |
| Google | `gemini-2.0-flash-live-preview-04-09` | `gemini` | PCM16 16kHz/24kHz |

> **Note**: `gpt-realtime` is OpenAI's latest realtime model with improved speech quality, emotion, and function calling capabilities.

## RealtimeAgent Builder

The `RealtimeAgentBuilder` provides a fluent API for configuring agents:

```rust
let agent = RealtimeAgent::builder("assistant")
    // Required
    .model(model)

    // Instructions (same as LlmAgent)
    .instruction("You are helpful.")
    .instruction_provider(|ctx| format!("User: {}", ctx.user_name()))

    // Voice settings
    .voice("alloy")  // Options: alloy, coral, sage, shimmer, etc.

    // Voice Activity Detection
    .server_vad()  // Use defaults
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

    // Callbacks (same as LlmAgent)
    .before_agent_callback(|ctx| async { Ok(()) })
    .after_agent_callback(|ctx, event| async { Ok(()) })
    .before_tool_callback(|ctx, tool, args| async { Ok(None) })
    .after_tool_callback(|ctx, tool, result| async { Ok(result) })

    // Realtime-specific callbacks
    .on_audio(|audio_chunk| { /* play audio */ })
    .on_transcript(|text| { /* show transcript */ })

    .build()?;
```

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

### Semantic VAD (Gemini)

For Gemini models, you can use semantic VAD which considers meaning:

```rust
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

When the model calls `transfer_to_agent`, the RealtimeRunner handles the handoff automatically.

## Audio Formats

| Format | Sample Rate | Bits | Channels | Use Case |
|--------|-------------|------|----------|----------|
| PCM16 | 24000 Hz | 16 | Mono | OpenAI (default) |
| PCM16 | 16000 Hz | 16 | Mono | Gemini input |
| G711 u-law | 8000 Hz | 8 | Mono | Telephony |
| G711 A-law | 8000 Hz | 8 | Mono | Telephony |

```rust
use adk_realtime::{AudioFormat, AudioChunk};

// Create audio format
let format = AudioFormat::pcm16_24khz();

// Work with audio chunks
let chunk = AudioChunk::new(audio_bytes, format);
let base64 = chunk.to_base64();
let decoded = AudioChunk::from_base64(&base64, format)?;
```

## Event Types

### Server Events

| Event | Description |
|-------|-------------|
| `SessionCreated` | Connection established |
| `AudioDelta` | Audio chunk (base64 PCM) |
| `TextDelta` | Text response chunk |
| `TranscriptDelta` | Input audio transcript |
| `FunctionCallDone` | Tool call request |
| `ResponseDone` | Response completed |
| `SpeechStarted` | VAD detected speech start |
| `SpeechStopped` | VAD detected speech end |
| `Error` | Error occurred |

### Client Events

| Event | Description |
|-------|-------------|
| `AudioInput` | Send audio chunk |
| `AudioCommit` | Commit audio buffer |
| `ItemCreate` | Send text or tool response |
| `CreateResponse` | Request a response |
| `CancelResponse` | Cancel current response |
| `SessionUpdate` | Update configuration |

## Examples

Run the included examples:

```bash
# Basic text-only session
cargo run --example realtime_basic --features realtime-openai

# Voice assistant with VAD
cargo run --example realtime_vad --features realtime-openai

# Tool calling
cargo run --example realtime_tools --features realtime-openai

# Multi-agent handoffs
cargo run --example realtime_handoff --features realtime-openai
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
| Tools | Function definitions | `Tool` trait + `ToolDefinition` |
| Handoffs | `transfer_to_agent` | `sub_agents` + auto-generated tool |
| Callbacks | Hooks | `before_*` / `after_*` callbacks |

---

**Previous**: [← Graph Agents](./graph-agents.md) | **Next**: [Model Providers →](../models/providers.md)