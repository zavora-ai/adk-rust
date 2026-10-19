# Desktop Audio Pipeline

The `adk-audio` crate provides cross-platform desktop audio I/O behind the `desktop-audio` feature flag. Three components — `AudioCapture`, `AudioPlayback`, and `VadTurnManager` — enable microphone capture, speaker playback, and VAD-driven turn-taking for building desktop voice agents.

## Overview

The desktop audio pipeline connects system audio hardware to the existing `adk-audio` pipeline system:

```
Microphone → AudioCapture → AudioStream → [VAD → STT → Agent → TTS] → AudioPlayback → Speaker
```

All components use the `cpal` crate for cross-platform audio (CoreAudio on macOS, PipeWire/ALSA/PulseAudio on Linux, WASAPI on Windows) and produce/consume the standard `AudioFrame` type.

## Feature Flags

```toml
[dependencies]
# Cross-platform (macOS, Linux, Windows) via cpal
adk-audio = { version = "3.0.0", features = ["desktop-audio"] }
```

The `desktop-audio` feature adds `cpal` as a dependency (and enables the no-op `vad` feature; `VadProcessor` is always compiled). It is not part of the `all` feature, since it needs audio hardware. On Linux it links ALSA, so install `libasound2-dev`.

### PipeWire Support

On modern Linux (Fedora 34+, Ubuntu 22.10+, Arch), PipeWire has replaced PulseAudio and ALSA as the default audio server. The `desktop-audio` feature works on PipeWire transparently through its ALSA compatibility layer — no extra configuration needed.

There is no native PipeWire backend; audio reaches PipeWire through ALSA.

## Quick Start

### List Audio Devices

```rust
use adk_audio::{AudioCapture, AudioPlayback};

let inputs = AudioCapture::list_input_devices()?;
for device in &inputs {
    println!("Mic: {} ({})", device.name(), device.id());
}

let outputs = AudioPlayback::list_output_devices()?;
for device in &outputs {
    println!("Speaker: {} ({})", device.name(), device.id());
}
```

### Capture Microphone Audio

```rust
use adk_audio::{AudioCapture, CaptureConfig};
use std::time::{Duration, Instant};

let mut capture = AudioCapture::new();
let devices = AudioCapture::list_input_devices()?;
let device = devices.first().expect("no input device");

let config = CaptureConfig::default(); // 16kHz, mono, 20ms frames
let mut stream = capture.start_capture(device.id(), &config)?;

let start = Instant::now();
while start.elapsed() < Duration::from_secs(3) {
    if let Some(frame) = stream.recv().await {
        // frame.data: PCM-16 LE bytes
        // frame.sample_rate: 16000
        // frame.channels: 1
        // frame.duration_ms: 20
    }
}
capture.stop_capture();
```

### Play Audio Through Speaker

```rust
use adk_audio::{AudioFrame, AudioPlayback};

let mut playback = AudioPlayback::new();
let devices = AudioPlayback::list_output_devices()?;
let device = devices.first().expect("no output device");

let frame = AudioFrame::silence(16000, 1, 1000); // 1 second
playback.play(device.id(), &frame).await?;
playback.stop();
```

### VAD Turn-Taking

```rust
use std::sync::Arc;
use adk_audio::{
    AudioCapture, AudioResult, CaptureConfig, VadConfig, VadMode,
    VadTurnManager, VoiceActivityEvent, VadProcessor,
};

// `vad` is your VadProcessor implementation; `device_id` comes from list_input_devices.
// Call it inside a Tokio runtime: the manager spawns a task.
fn start_turn_taking(
    vad: Arc<dyn VadProcessor>,
    capture: &mut AudioCapture,
    device_id: &str,
) -> AudioResult<VadTurnManager> {
    let config = VadConfig {
        mode: VadMode::HandsFree,
        silence_threshold_ms: 500,
        speech_threshold_ms: 200,
    };

    let mut manager = VadTurnManager::new(vad, config)?;
    let stream = capture.start_capture(device_id, &CaptureConfig::default())?;

    manager.start(stream, |event| {
        match event {
            VoiceActivityEvent::SpeechStarted => println!("Speech started"),
            VoiceActivityEvent::SpeechEnded { duration_ms } => {
                println!("Speech ended ({duration_ms}ms)");
            }
        }
    });
    Ok(manager)
}
```

## Components

### AudioDevice

Descriptor for a system audio device (input or output). Contains an opaque `id` and a human-readable `name`.

### CaptureConfig

Configuration for microphone capture:

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `sample_rate` | `u32` | 16000 | Sample rate in Hz |
| `channels` | `u8` | 1 | Channel count (1=mono, 2=stereo) |
| `frame_duration_ms` | `u32` | 20 | Duration of each AudioFrame |

Call `validate()` before use — rejects zero values with `AudioError::Device`.

### AudioCapture

Microphone capture via `cpal`. The `start_capture()` method returns an `AudioStream` (bounded `mpsc::Receiver<AudioFrame>` with capacity 64). Frames are produced at `frame_duration_ms` intervals in PCM-16 LE format.

### AudioPlayback

Speaker playback via `cpal`. The `play()` method queues an `AudioFrame`'s samples into a shared buffer that the cpal output callback drains. Call `stop()` to release the device.

### VadTurnManager

Consumes an `AudioStream`, applies `VadProcessor::is_speech()` to each frame, and emits `VoiceActivityEvent` values via a registered callback.

Two modes:
- **HandsFree** — automatic speech boundary detection using configurable silence and speech duration thresholds
- **PushToTalk** — no automatic events; the caller controls gating externally

### VadConfig

| Field | Type | Description |
|-------|------|-------------|
| `mode` | `VadMode` | `HandsFree` or `PushToTalk` |
| `silence_threshold_ms` | `u32` | Consecutive silence before `SpeechEnded` |
| `speech_threshold_ms` | `u32` | Consecutive speech before `SpeechStarted` |

Call `validate()` before use — rejects zero thresholds with `AudioError::Vad`.

## Building a Voice Agent

The full conversational voice agent pattern:

1. Capture audio from microphone
2. Detect speech boundaries with VAD
3. Transcribe speech with GeminiStt (or any `SttProvider`)
4. Send transcript to an LlmAgent for reasoning
5. Synthesize response with GeminiTts (or any `TtsProvider`)
6. Play synthesized audio through speaker

See `examples/desktop_audio/src/voice_agent.rs` for a complete example using the Gemini STT and TTS providers.

## Thread Safety

All desktop audio types (`AudioCapture`, `AudioPlayback`, `VadTurnManager`) are `Send + Sync`, making them safe to share across Tokio tasks.

## Error Handling

Desktop audio errors use the existing `AudioError` enum:

| Component | Error Variant | When |
|-----------|--------------|------|
| AudioCapture | `AudioError::Device` | Device not found, host unavailable, config validation |
| AudioPlayback | `AudioError::Device` | Device not found, host unavailable, open/write failure |
| VadTurnManager | `AudioError::Vad` | Config validation (zero thresholds) |

## Platform Support

| Platform | Audio Backend | Status |
|----------|--------------|--------|
| macOS | CoreAudio | Supported |
| Linux | PipeWire (via ALSA compat) | Supported (install `libasound2-dev`) |
| Linux | ALSA / PulseAudio (legacy) | Supported (install `libasound2-dev`) |
| Windows | WASAPI | Supported |

On modern Linux, PipeWire is the default audio server. The `desktop-audio` feature reaches it through PipeWire's ALSA compatibility layer.

## Examples

`examples/desktop_audio/` has six programs — `list-devices`, `capture-audio`, `playback-audio`, `vad-turn-taking`, `config-validation`, and `voice-agent`, a conversational voice agent with Gemini STT and TTS:

```bash
cargo run --manifest-path examples/desktop_audio/Cargo.toml --bin voice-agent
```
