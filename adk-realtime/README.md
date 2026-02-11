# adk-realtime

Real-time bidirectional audio streaming for Rust Agent Development Kit (ADK-Rust) agents.

## Overview

`adk-realtime` provides a unified interface for building voice-enabled AI agents using real-time streaming APIs from various providers. It follows the **OpenAI Agents SDK pattern** with a separate, decoupled implementation that integrates seamlessly with the ADK agent ecosystem.

## Features

- **RealtimeAgent**: Implements `adk_core::Agent` with full callback/tool/instruction support
- **Multiple Providers**: Support for OpenAI Realtime API and Gemini Live API
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
| Google | `gemini-2.0-flash-live-preview-04-09` | `gemini` | Gemini Live API |

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
                // Play audio (delta is base64-encoded PCM)
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
| `on_audio(callback)` | Callback for audio output events |
| `on_transcript(callback)` | Callback for transcript events |
| `on_speech_started(callback)` | Callback when speech detected |
| `on_speech_stopped(callback)` | Callback when speech ends |

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

## Feature Flags

| Flag | Description |
|------|-------------|
| `openai` | Enable OpenAI Realtime API |
| `gemini` | Enable Gemini Live API |
| `full` | Enable all providers |

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
