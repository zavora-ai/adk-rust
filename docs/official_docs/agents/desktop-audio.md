# Desktop Audio Pipeline

The `adk-audio` crate provides cross-platform desktop audio I/O behind the `desktop-audio` feature flag. Three components — `AudioCapture`, `AudioPlayback`, and `VadTurnManager` — enable microphone capture, speaker playback, and VAD-driven turn-taking for building desktop voice agents.

## Overview

The desktop audio pipeline connects system audio hardware to the existing `adk-audio` pipeline system:

```
Microphone → AudioCapture → AudioStream → [VAD → STT → Agent → TTS] → AudioPlayback → Speaker
```

All components use the `cpal` crate for cross-platform audio (CoreAudio on macOS, ALSA/PulseAudio on Linux, WASAPI on Windows) and produce/consume the standard `AudioFrame` type.

## Feature Flag

```toml
[dependencies]
adk-audio = { version = "0.7.0", features = ["desktop-audio"] }
```

The `desktop-audio` feature implies `vad` (for `VadProcessor`) and adds `cpal` as a dependency. It is intentionally excluded from the `all` feature to avoid pulling platform-specific audio dependencies into CI builds without audio hardware.

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
    AudioCapture, CaptureConfig, VadConfig, VadMode,
    VadTurnManager, VoiceActivityEvent, VadProcessor,
};

let vad: Arc<dyn VadProcessor> = /* your VadProcessor impl */;
let config = VadConfig {
    mode: VadMode::HandsFree,
    silence_threshold_ms: 500,
    speech_threshold_ms: 200,
};

let mut manager = VadTurnManager::new(vad, config)?;
let mut capture = AudioCapture::new();
let stream = capture.start_capture(device_id, &CaptureConfig::default())?;

manager.start(stream, |event| {
    match event {
        VoiceActivityEvent::SpeechStarted => println!("Speech started"),
        VoiceActivityEvent::SpeechEnded { duration_ms } => {
            println!("Speech ended ({duration_ms}ms)");
        }
    }
});
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

See `examples/desktop_audio/src/voice_agent.rs` for a complete working example using real Gemini cloud providers.

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
| Linux | ALSA / PulseAudio | Supported (install `libasound2-dev`) |
| Windows | WASAPI | Supported |

## Examples

See `examples/desktop_audio/` for 6 practical examples including a full conversational voice agent with real Gemini STT/TTS.
