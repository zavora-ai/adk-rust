# adk-audio

Audio intelligence and pipeline orchestration for ADK-Rust agents.

Provides unified traits for Text-to-Speech (TTS), Speech-to-Text (STT), music generation, audio FX/DSP processing, and Voice Activity Detection (VAD), with a composable pipeline system for building voice agent loops, podcast production, transcription, and generative soundscapes.

## Features

| Feature | Description | Dependencies |
|---------|-------------|--------------|
| `tts` (default) | Cloud TTS providers (ElevenLabs, OpenAI, Gemini, Cartesia) | `reqwest`, `base64` |
| `stt` (default) | Cloud STT providers (Whisper API, Deepgram, AssemblyAI) | `reqwest`, `tokio-tungstenite` |
| `music` | Music generation providers | `reqwest` |
| `fx` | DSP processors (normalizer, resampler, noise, compressor, trimmer, pitch) | `rubato`, `dasp` |
| `vad` | Voice Activity Detection | `webrtc-vad` |
| `opus` | Opus codec encode/decode | `audiopus` (requires cmake) |
| `mlx` | Apple Silicon local inference (macOS only) | `mlx-rs`, `tokenizers`, `hf-hub` |
| `onnx` | ONNX Runtime local inference (cross-platform) | `ort`, `tokenizers`, `hf-hub` |
| `livekit` | adk-realtime bridge | `livekit-api`, `adk-realtime` |
| `all` | All non-platform features (no mlx/onnx) | — |

## Quick Start

### Cloud TTS

```rust,ignore
use adk_audio::{ElevenLabsTts, TtsProvider, TtsRequest};

let tts = ElevenLabsTts::from_env()?;
let request = TtsRequest {
    text: "Hello from ADK Audio!".into(),
    voice: "Rachel".into(),
    ..Default::default()
};
let frame = tts.synthesize(&request).await?;
println!("Generated {} ms of audio", frame.duration_ms);
```

### Cloud STT

```rust,ignore
use adk_audio::{WhisperApiStt, SttProvider, SttOptions};

let stt = WhisperApiStt::from_env()?;
let transcript = stt.transcribe(&audio_frame, &SttOptions::default()).await?;
println!("Transcript: {}", transcript.text);
```

### Pipeline

```rust,ignore
use adk_audio::AudioPipelineBuilder;

let handle = AudioPipelineBuilder::new()
    .tts(my_tts_provider)
    .build_tts()?;
```

## Cloud Providers

### TTS
- **ElevenLabs** — High-quality multilingual voices (`ELEVENLABS_API_KEY`)
- **OpenAI** — TTS-1 and TTS-1-HD models (`OPENAI_API_KEY`)
- **Gemini** — Native audio via generateContent (`GEMINI_API_KEY`)
- **Cartesia** — Sonic-2 low-latency streaming (`CARTESIA_API_KEY`)

### STT
- **Whisper API** — OpenAI Whisper transcription (`OPENAI_API_KEY`)
- **Deepgram** — Nova-2 with diarization and streaming (`DEEPGRAM_API_KEY`)
- **AssemblyAI** — Universal model with async jobs and streaming (`ASSEMBLYAI_API_KEY`)

## Local Inference

### MLX (Apple Silicon)

Runs TTS and STT models on Metal GPU with zero-copy unified memory:

```rust,ignore
use adk_audio::mlx::{MlxTtsProvider, MlxTtsConfig};

let tts = MlxTtsProvider::default_kokoro().await?;
```

### ONNX (Cross-Platform)

Runs TTS models via ONNX Runtime with CUDA, CoreML, or CPU:

```rust,ignore
use adk_audio::onnx::{OnnxTtsProvider, OnnxModelConfig};

let tts = OnnxTtsProvider::default_kokoro().await?;
```

## DSP Processors

Behind the `fx` feature:
- `LoudnessNormalizer` — EBU R128 loudness normalization
- `Resampler` — Sample rate conversion (8kHz–96kHz)
- `NoiseSuppressor` — Spectral noise reduction
- `DynamicRangeCompressor` — Dynamic range compression
- `SilenceTrimmer` — Leading/trailing silence removal
- `PitchShifter` — Voice pitch adjustment

## License

See [LICENSE](../LICENSE) in the repository root.
