# Desktop Audio Examples

Practical examples exercising the full `adk-audio` desktop audio pipeline: device enumeration, microphone capture, speaker playback, VAD turn-taking, a complete conversational voice agent with real Gemini STT/TTS, and configuration validation.

## Prerequisites

- **Rust 1.85+** (edition 2024)
- **Audio hardware** — microphone and speakers (all examples except `config-validation`)
- **GEMINI_API_KEY** — required for the `voice-agent` example ([get a key](https://aistudio.google.com/apikey))

## Setup

```bash
# Copy and edit the environment file (only needed for voice-agent)
cp examples/desktop_audio/.env.example examples/desktop_audio/.env
# Edit .env and add your Gemini API key
```

## Examples

### list-devices

Enumerate all system input (microphone) and output (speaker) devices.

```bash
cargo run --manifest-path examples/desktop_audio/Cargo.toml --bin list-devices
```

**No API key required.** Shows device names and IDs.

```
=== Desktop Audio Device Enumeration ===

Input Devices (Microphones):
  [0] Built-in Microphone (id: Built-in Microphone)

Output Devices (Speakers):
  [0] Built-in Output (id: Built-in Output)

Total: 1 input, 1 output
```

### capture-audio

Capture audio from the default microphone for 3 seconds and print frame statistics.

```bash
cargo run --manifest-path examples/desktop_audio/Cargo.toml --bin capture-audio
```

**No API key required.** Requires a microphone.

```
=== Microphone Capture Demo ===

Using device: Built-in Microphone (Built-in Microphone)
Config: 16000Hz, 1ch, 20ms frames

Capturing for 3 seconds...
  Frame 50: 16000Hz, 1ch, 20ms, 640 bytes
  Frame 100: 16000Hz, 1ch, 20ms, 640 bytes

Capture complete: 148 frames in 3.0s
```

### playback-audio

Play 1 second of silence through the default speaker.

```bash
cargo run --manifest-path examples/desktop_audio/Cargo.toml --bin playback-audio
```

**No API key required.** Requires speakers.

```
=== Speaker Playback Demo ===

Using device: Built-in Output (Built-in Output)
Playing 1 second of silence (16000Hz, 1ch, 32000 bytes)...
Playback complete.
```

### vad-turn-taking

VAD-driven turn-taking in HandsFree mode. Listens for 10 seconds and prints speech start/end events as you speak.

```bash
cargo run --manifest-path examples/desktop_audio/Cargo.toml --bin vad-turn-taking
```

**No API key required.** Requires a microphone.

```
=== VAD Turn-Taking Demo (HandsFree) ===

Using device: Built-in Microphone
Listening for 10 seconds... speak into your microphone.

🎙️  Speech started
🔇 Speech ended (duration: 1580 ms)

VAD demo complete.
```

### voice-agent ⭐

The crown jewel — a fully conversational voice agent. You speak naturally, the agent listens, thinks, and responds with synthesized speech through your speaker. Uses real Gemini cloud providers for everything:

- **GeminiStt** (`gemini-3-flash-preview`) — real cloud speech-to-text
- **LlmAgent** (`gemini-2.5-flash`) — real conversational reasoning
- **GeminiTts** (`gemini-3.1-flash-tts-preview`) — real cloud text-to-speech at 24kHz

```bash
cargo run --manifest-path examples/desktop_audio/Cargo.toml --bin voice-agent
```

**Requires `GEMINI_API_KEY`** (set in `.env` or environment).

The agent runs for 60 seconds. Speak into your microphone and it will respond through your speaker:

```
=== Voice Agent with Real Gemini STT/TTS ===

✅ Gemini STT initialized (gemini-3-flash-preview)
✅ Gemini TTS initialized (gemini-3.1-flash-tts-preview)
✅ LlmAgent initialized (gemini-2.5-flash)
✅ Runner initialized

🎤 Input:  Built-in Microphone
🔊 Output: Built-in Output

💬 Speak into your microphone. The agent will respond.

   (Running for 60 seconds — Ctrl+C to stop early)

🎙️  Listening...
   (920ms of audio captured)
📝 Transcribing... "Hello."
🤔 Thinking...
🤖 Agent: "Hi there! How can I help you today?"
🔊 Speaking... (2480ms audio at 24000Hz)

🎙️  Listening...
   (1940ms of audio captured)
📝 Transcribing... "Um, my name is James."
🤔 Thinking...
🤖 Agent: "Got it, James. What can I help you with today?"
🔊 Speaking... (3000ms audio at 24000Hz)

🎙️  Listening...
   (2280ms of audio captured)
📝 Transcribing... "Tell me about ADK Rust."
🤔 Thinking...
🤖 Agent: "ADK Rust is a modular toolkit for building AI agents..."
🔊 Speaking... (4200ms audio at 24000Hz)

--- Voice agent complete (3 turns) ---
```

**How it works:**

1. Microphone captures audio at 16kHz mono via `cpal`
2. Local `MockVad` detects speech boundaries by amplitude
3. When you stop speaking (600ms silence), audio is sent to **GeminiStt** for transcription
4. Your transcript goes to the **LlmAgent** which generates a conversational response
5. The response is synthesized by **GeminiTts** into 24kHz audio
6. Audio plays through your speaker via `cpal`
7. The agent maintains conversation context across turns via `adk-session`

### config-validation

Demonstrates configuration validation and error handling for `CaptureConfig` and `VadConfig`.

```bash
cargo run --manifest-path examples/desktop_audio/Cargo.toml --bin config-validation
```

**No API key required.** No audio hardware required.

```
=== Configuration Validation Demo ===

--- CaptureConfig ---

✗ Zero sample rate: Device error: invalid sample rate: 0
✗ Zero channels:    Device error: invalid channel count: 0
✗ Zero duration:    Device error: invalid frame duration: 0 ms
✓ Valid config:     16000Hz, 1ch, 20ms

--- VadConfig ---

✗ Zero silence threshold: VAD error: invalid silence threshold: 0 ms.
✗ Zero speech threshold:  VAD error: invalid speech threshold: 0 ms.
✓ Valid VAD config:       HandsFree, 500ms silence, 200ms speech

Validation demo complete.
```

## Running Tests

```bash
# Unit tests (MockVad)
cargo test --manifest-path examples/desktop_audio/Cargo.toml --lib

# Property tests (MockVad amplitude classification, config validation)
cargo test --manifest-path examples/desktop_audio/Cargo.toml --test mock_property_tests
```

## Troubleshooting

| Problem | Solution |
|---------|----------|
| "no input devices found" | Connect a microphone and check OS audio settings |
| "no output devices found" | Connect speakers or headphones |
| "GEMINI_API_KEY or GOOGLE_API_KEY not set" | Copy `.env.example` to `.env` and add your key |
| Build errors with `cpal` on Linux | Install ALSA headers: `sudo apt install libasound2-dev` |
| STT returns empty transcripts | Speak louder or closer to the mic; check mic isn't muted |
| TTS audio sounds distorted | Your output device may not support 24kHz — try a different device |

## Architecture

```
examples/desktop_audio/
├── Cargo.toml              # Standalone workspace crate
├── .env.example            # GEMINI_API_KEY template
├── README.md               # This file
├── src/
│   ├── lib.rs              # MockVad, setup_tracing, print helpers
│   ├── list_devices.rs     # Device enumeration
│   ├── capture_audio.rs    # Microphone capture (3s)
│   ├── playback_audio.rs   # Speaker playback (silence)
│   ├── vad_turn_taking.rs  # VAD HandsFree mode (10s)
│   ├── voice_agent.rs      # Full conversational voice agent (60s)
│   └── config_validation.rs # Config validation demo
└── tests/
    └── mock_property_tests.rs  # Property-based tests
```

## Components Used

| Component | Source | Purpose |
|-----------|--------|---------|
| `AudioCapture` | `adk-audio` | Microphone input via cpal |
| `AudioPlayback` | `adk-audio` | Speaker output via cpal |
| `VadTurnManager` | `adk-audio` | Speech boundary detection |
| `GeminiStt` | `adk-audio` | Cloud speech-to-text |
| `GeminiTts` | `adk-audio` | Cloud text-to-speech |
| `LlmAgent` | `adk-agent` | Conversational reasoning |
| `GeminiModel` | `adk-model` | Gemini LLM provider |
| `Runner` | `adk-runner` | Agent execution with session context |
| `InMemorySessionService` | `adk-session` | Conversation history |
| `MockVad` | local | Amplitude-based VAD (no cloud needed) |
