# Realtime Agent Test Examples

This project contains working examples that demonstrate the key concepts from the realtime-agents.md documentation.

## Examples

| Example | Description | Key Concepts |
|---------|-------------|--------------|
| `basic_realtime` | Simple text-based realtime session | Session API, events, text input/output |
| `realtime_with_tools` | Tool calling during realtime session | Function definitions, tool responses |
| `realtime_vad` | Voice activity detection configuration | VAD settings, speech detection |
| `realtime_handoff` | Multi-agent handoffs | Sub-agents, transfer_to_agent |

## Setup

1. Set your API key:
```bash
export OPENAI_API_KEY=your-api-key
```

2. Run examples:
```bash
# Basic text session
cargo run --bin basic_realtime

# Tool calling
cargo run --bin realtime_with_tools

# VAD configuration
cargo run --bin realtime_vad

# Multi-agent handoffs
cargo run --bin realtime_handoff
```

## Notes

- These examples use **text mode** for easier testing (no microphone required)
- For actual voice interactions, you would send audio chunks via `session.send_audio()`
- The OpenAI Realtime API requires a valid API key with realtime access

## Key Learning Points

1. **RealtimeConfig**: Configure sessions with instructions, voice, and tools
2. **ServerEvent**: Handle different event types (TextDelta, AudioDelta, FunctionCallDone)
3. **VAD**: Voice Activity Detection for natural conversation flow
4. **Tool Calling**: Execute functions during realtime sessions
5. **Handoffs**: Transfer conversations between specialized agents
