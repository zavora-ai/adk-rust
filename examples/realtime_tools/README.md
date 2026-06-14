# Realtime Tools — Function Calling in Voice Conversations

Demonstrates **server-side tool execution** during OpenAI Realtime voice sessions using `IntegratedRealtimeRunner` with full ADK integration.

## What This Shows

| Feature | How |
|---------|-----|
| **IntegratedRealtimeRunner** | Wraps `RealtimeRunner` with session, memory, and transcript aggregation |
| **Tool auto-dispatch** | Model requests tool → server executes → result sent back → model speaks it |
| **SessionService** | Each turn (user + assistant) persisted to `InMemorySessionService` |
| **MemoryService** | Completed turns stored for future semantic retrieval |
| **TranscriptAggregator** | Streaming deltas assembled into complete turns automatically |
| **GA API protocol** | `gpt-realtime-2`, nested `audio.input`/`audio.output`, server VAD |
| **Multi-tool turns** | Model can call multiple tools in a single response |

## Architecture

```text
                   IntegratedRealtimeRunner
                   │
   ┌───────────────┼───────────────┐
   │               │               │
   ▼               ▼               ▼
RealtimeRunner   SessionService   MemoryService
(OpenAI WS)      (persist turns)  (store insights)
   │
   ├─ send_text("What's the weather in Tokyo?")
   ├─ create_response()
   ├─ next_event() → FunctionCallDone { name: "get_weather" }
   │   └─ auto-executes tool, sends result back
   ├─ next_event() → TranscriptDelta { delta: "It's 75°F..." }
   ├─ next_event() → ResponseDone
   │   └─ TranscriptAggregator emits TurnComplete
   │   └─ SessionService.append_event(assistant turn)
   │   └─ MemoryService.add_session(turn content)
   └─ done
```

## Run

```bash
cargo run --manifest-path examples/realtime_tools/Cargo.toml
```

Requires: `OPENAI_API_KEY` environment variable.

## Expected Output

```
╔══════════════════════════════════════════════════════════════╗
║  Realtime Tools — Function Calling via IntegratedRunner      ║
╚══════════════════════════════════════════════════════════════╝

📡 Connecting to OpenAI Realtime (gpt-realtime-2)...
✅ Connected — session abc123

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  Turn 1: Single Tool Call
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

👤 What's the weather in Tokyo right now?

  🌤️  get_weather("Tokyo")
🤖 It's currently 75°F and partly cloudy in Tokyo.

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  Turn 2: Multi-Tool Call
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

👤 What's the weather in London and what time is it there?

  🌤️  get_weather("London")
  🕐 get_time("GMT")
🤖 London is 58°F and overcast. It's currently 12:30 AM there.
```

## Comparison with Old Example

| Aspect | Old (Beta API) | New (GA API + Integration) |
|--------|---------------|---------------------------|
| Model | `gpt-4o-mini-realtime-preview-2024-12-17` | `gpt-realtime-2` |
| Runner | `RealtimeRunner` (raw) | `IntegratedRealtimeRunner` |
| Persistence | None | SessionService + MemoryService |
| Transcript | Manual delta collection | TranscriptAggregator (automatic) |
| Protocol | Beta (`OpenAI-Beta: realtime=v1`) | GA (nested audio config) |
| Status | ❌ Shut down | ✅ Production |
