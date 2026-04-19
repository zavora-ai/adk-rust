# adk-audio

Audio intelligence and pipeline orchestration for ADK-Rust agents.

Provides unified traits for Text-to-Speech (TTS), Speech-to-Text (STT), music generation, audio FX/DSP processing, and Voice Activity Detection (VAD), with a composable pipeline system for building voice agent loops, podcast production, transcription, and generative soundscapes.

## Installation

```toml
[dependencies]
adk-audio = "0.6.0"
```

Or via the umbrella crate (experimental):

```toml
[dependencies]
adk-rust = { version = "0.6.0", features = ["audio"] }
```

## Feature Flags

| Feature | Description | Key dependencies |
|---------|-------------|------------------|
| `tts` (default) | Cloud TTS providers (ElevenLabs, OpenAI, Gemini, Cartesia) | `reqwest`, `base64` |
| `stt` (default) | Cloud STT providers (Whisper API, Deepgram, AssemblyAI) | `reqwest`, `tokio-tungstenite` |
| `music` | Music generation providers | `reqwest` |
| `fx` | DSP processors (normalizer, resampler, noise, compressor, trimmer, pitch) | `rubato`, `dasp` |
| `vad` | Voice Activity Detection | `webrtc-vad` |
| `mlx` | Local inference model loading (tokenizers + HF Hub, cross-platform) | `tokenizers`, `hf-hub` |
| `onnx` | ONNX Runtime local inference (cross-platform) | `ort`, `tokenizers`, `hf-hub` |
| `kokoro` | Kokoro-82M ONNX TTS with espeak-ng phonemizer | `espeak-rs`, `ndarray` (implies `onnx`) |
| `chatterbox` | Chatterbox ONNX TTS | implies `onnx` |
| `whisper-onnx` | Whisper ONNX STT (base/small/medium/large) | implies `onnx` |
| `distil-whisper` | Distil-Whisper ONNX STT | implies `onnx` |
| `moonshine` | Moonshine ONNX STT | implies `onnx` |
| `qwen3-tts` | Qwen3-TTS native Candle-based TTS (0.6B / 1.7B) | `qwen_tts`, `candle-core`, `hf-hub` |
| `all-onnx` | All ONNX backends (STT + TTS) | combines above |
| `livekit` | adk-realtime bridge | `livekit-api`, `adk-realtime` |
| `desktop-audio` | Desktop mic capture, speaker playback, VAD turn-taking (PipeWire/ALSA/CoreAudio/WASAPI) | `cpal` (implies `vad`) |
| `streaming` | Streaming support marker | — |
| `all` | All portable features — safe for CI on any platform | everything above |

## Core Types

### AudioFrame

The canonical audio buffer used throughout the crate — raw PCM-16 LE samples with metadata:

```rust
use adk_audio::AudioFrame;

// Create from raw PCM data (duration computed automatically)
let frame = AudioFrame::new(pcm_bytes, 16000, 1); // 16kHz mono
println!("{}ms of audio", frame.duration_ms);

// Access raw i16 samples
let samples: &[i16] = frame.samples();

// Generate silence
let silence = AudioFrame::silence(24000, 1, 500); // 500ms at 24kHz

// Merge multiple frames into one
let merged = adk_audio::merge_frames(&[frame1, frame2, frame3]);
```

### Codec

Encode/decode between `AudioFrame` and external formats:

```rust
use adk_audio::{AudioFormat, encode, decode};

// Encode to WAV
let wav_bytes = encode(&frame, AudioFormat::Wav)?;

// Decode from WAV
let frame = decode(&wav_data, AudioFormat::Wav)?;
```

Currently supports `Pcm16` (passthrough) and `Wav`. Other formats (`Opus`, `Mp3`, `Flac`, `Ogg`) are defined but not yet implemented — check `AudioFormat::supports_encode()` / `supports_decode()`.

## Cloud Providers

### TTS Providers

All cloud TTS providers implement the `TtsProvider` trait with `synthesize()` (batch) and `synthesize_stream()` (streaming) methods.

| Provider | Type | Env var | Feature |
|----------|------|---------|---------|
| `ElevenLabsTts` | High-quality multilingual voices | `ELEVENLABS_API_KEY` | `tts` |
| `OpenAiTts` | TTS-1 and TTS-1-HD models | `OPENAI_API_KEY` | `tts` |
| `GeminiTts` | Native audio via generateContent | `GEMINI_API_KEY` | `tts` |
| `CartesiaTts` | Sonic-2 low-latency streaming | `CARTESIA_API_KEY` | `tts` |

```rust
use adk_audio::{ElevenLabsTts, TtsProvider, TtsRequest};

let tts = ElevenLabsTts::from_env()?;
let request = TtsRequest {
    text: "Hello from ADK Audio!".into(),
    voice: "Rachel".into(),
    speed: 1.0,
    ..Default::default()
};
let frame = tts.synthesize(&request).await?;
println!("Generated {} ms of audio", frame.duration_ms);
```

`TtsRequest` also supports optional `language`, `pitch`, `emotion` (enum: `Neutral`, `Happy`, `Sad`, `Angry`, `Whisper`, `Excited`, `Calm`), and `output_format` fields.

All cloud providers accept a `CloudTtsConfig` for API key and optional base URL override.

### STT Providers

All cloud STT providers implement the `SttProvider` trait with `transcribe()` (batch) and `transcribe_stream()` (streaming) methods.

| Provider | Type | Env var | Feature |
|----------|------|---------|---------|
| `WhisperApiStt` | OpenAI Whisper transcription | `OPENAI_API_KEY` | `stt` |
| `DeepgramStt` | Nova-2 with diarization and streaming | `DEEPGRAM_API_KEY` | `stt` |
| `AssemblyAiStt` | Universal model with async jobs and streaming | `ASSEMBLYAI_API_KEY` | `stt` |

```rust
use adk_audio::{WhisperApiStt, SttProvider, SttOptions};

let stt = WhisperApiStt::from_env()?;
let opts = SttOptions {
    language: Some("en".into()),
    diarize: true,
    word_timestamps: true,
    ..Default::default()
};
let transcript = stt.transcribe(&audio_frame, &opts).await?;
println!("{}", transcript.text);
```

The `Transcript` result includes `text`, per-`Word` timestamps with confidence, `Speaker` diarization, overall `confidence`, and `language_detected`.

## Local Inference

### ONNX TTS (cross-platform)

Runs TTS models via ONNX Runtime with CUDA, CoreML, or CPU execution providers:

```rust
use adk_audio::{OnnxTtsProvider, OnnxModelConfig};

let tts = OnnxTtsProvider::default_kokoro().await?;
```

The `OnnxTtsProvider` is generic over a `Preprocessor` trait:
- `TokenizerPreprocessor` — default, uses HuggingFace `tokenizer.json`
- `KokoroPreprocessor` — espeak-ng phonemizer for Kokoro-82M (requires `kokoro` feature + system espeak-ng)

Execution providers: `OnnxExecutionProvider::Cpu`, `Cuda`, `CoreMl`, `DirectMl`.

### ONNX STT (cross-platform)

Three STT backends behind separate feature flags:

| Backend | Feature | Type |
|---------|---------|------|
| Whisper (base/small/medium/large) | `whisper-onnx` | `SttBackend::Whisper(WhisperModelSize)` |
| Distil-Whisper (small/medium/large-v3) | `distil-whisper` | `SttBackend::DistilWhisper(DistilWhisperVariant)` |
| Moonshine (tiny/base) | `moonshine` | `SttBackend::Moonshine(MoonshineVariant)` |

```rust
use adk_audio::{OnnxSttProvider, OnnxSttConfig, SttBackend, WhisperModelSize};

let config = OnnxSttConfig::builder()
    .backend(SttBackend::Whisper(WhisperModelSize::Base))
    .build();
let stt = OnnxSttProvider::new(config, &registry).await?;
// Or use the default:
let stt = OnnxSttProvider::default_whisper().await?;
```

### MLX (Apple Silicon)

Local TTS and STT using tokenizers + HF Hub. Full Metal GPU inference via `mlx-rs` is planned.

```rust
use adk_audio::{MlxTtsProvider, MlxTtsConfig};

let tts = MlxTtsProvider::default_kokoro().await?;
```

Configurable via `MlxTtsConfig` and `MlxSttConfig`, with `MlxQuantization` options.

### Qwen3-TTS (Candle-based)

Native Candle-based TTS supporting 10 languages with predefined speaker voices. Runs on CPU, Metal (macOS), or CUDA.

```rust
use adk_audio::{Qwen3TtsNativeProvider, Qwen3TtsVariant, TtsRequest};

let tts = Qwen3TtsNativeProvider::new(Qwen3TtsVariant::Small).await?; // 0.6B
let request = TtsRequest {
    text: "Hello!".into(),
    voice: "vivian".into(), // or "lang:zh", or "vivian:zh"
    ..Default::default()
};
let frame = tts.synthesize(&request).await?;
```

Variants: `Small` (0.6B, faster) and `Large` (1.7B, higher quality). Predefined speakers: `vivian`, `serena`, `dylan`, `eric`, `ryan`, `aiden` (en), `uncle_fu` (zh), `ono_anna` (ja), `sohee` (ko).

### Chatterbox TTS

ONNX-based TTS via the `chatterbox` feature:

```rust
use adk_audio::{ChatterboxTtsProvider, ChatterboxConfig, ChatterboxVariant};
```

### Model Registry

`LocalModelRegistry` handles downloading and caching model weights from HuggingFace Hub:

```rust
use adk_audio::LocalModelRegistry;

let registry = LocalModelRegistry::default(); // ~/.cache/adk-audio/models/
let path = registry.get_or_download("onnx-community/whisper-base").await?;
```

Custom cache directory: `LocalModelRegistry::new("/my/cache")`.

## DSP Processors

Behind the `fx` feature. All implement the `AudioProcessor` trait.

| Processor | Description |
|-----------|-------------|
| `LoudnessNormalizer` | EBU R128 loudness normalization |
| `Resampler` | Sample rate conversion (8kHz–96kHz) |
| `NoiseSuppressor` | Spectral noise reduction |
| `DynamicRangeCompressor` | Dynamic range compression |
| `SilenceTrimmer` | Leading/trailing silence removal |
| `PitchShifter` | Voice pitch adjustment |

### FxChain

Chain processors in series — output of stage N feeds into stage N+1:

```rust
use adk_audio::{FxChain, LoudnessNormalizer, Resampler, AudioProcessor};

let chain = FxChain::new()
    .push(LoudnessNormalizer::new(-16.0))
    .push(Resampler::new(24000));
let output = chain.process(&input_frame).await?;
```

## Mixer

Multi-track audio mixer with per-track volume control:

```rust
use adk_audio::Mixer;

let mut mixer = Mixer::new(24000);
mixer.add_track("narration", 1.0);
mixer.add_track("music", 0.3);
mixer.push_frame("narration", narration_frame);
mixer.push_frame("music", music_frame);
let mixed = mixer.mix()?;
```

## Pipeline System

The `AudioPipelineBuilder` composes providers, processors, and agents into async processing topologies. Each pipeline returns a `PipelineHandle` with `input_tx` / `output_rx` channels, real-time `metrics`, and a `shutdown()` method.

### Pipeline Topologies

| Builder method | Flow | Required components |
|----------------|------|---------------------|
| `build_tts()` | Text → TTS → Audio | `tts` |
| `build_stt()` | Audio → STT → Transcript | `stt` |
| `build_voice_agent()` | Audio → VAD → STT → Agent → TTS → Audio | `tts`, `stt`, `vad`, `agent` |
| `build_transform()` | Audio → FxChain → Audio | `pre_fx` (optional) |
| `build_music()` | Text → MusicProvider → Audio | `music` |

```rust
use adk_audio::{AudioPipelineBuilder, PipelineInput, PipelineOutput};

let mut handle = AudioPipelineBuilder::new()
    .tts(my_tts)
    .stt(my_stt)
    .vad(my_vad)
    .agent(my_agent)
    .pre_fx(pre_chain)   // optional: applied before STT
    .post_fx(post_chain)  // optional: applied after TTS
    .buffer_size(64)      // channel buffer (default 32)
    .build_voice_agent()?;

// Send input
handle.input_tx.send(PipelineInput::Audio(frame)).await?;
handle.input_tx.send(PipelineInput::Text("Hello".into())).await?;

// Receive output
if let Some(output) = handle.output_rx.recv().await {
    match output {
        PipelineOutput::Audio(frame) => { /* synthesized audio */ }
        PipelineOutput::Transcript(t) => { /* STT result */ }
        PipelineOutput::AgentText(text) => { /* agent response before TTS */ }
        PipelineOutput::Metrics(m) => { /* pipeline metrics */ }
    }
}

// Shutdown
handle.shutdown();
```

### SentenceChunker

Buffers LLM tokens and emits complete sentences at delimiter boundaries (`.!?;\n`), reducing time-to-first-audio in voice agent pipelines:

```rust
use adk_audio::SentenceChunker;

let mut chunker = SentenceChunker::new();
let sentences = chunker.push("Hello world. How are ");
// sentences == ["Hello world."]
let more = chunker.push("you? Fine.");
// more == ["How are you?", "Fine."]
let remaining = chunker.flush();
// remaining == None (buffer empty)
```

### Preset Pipelines

Factory functions for common topologies:

```rust
use adk_audio::pipeline::presets::*;

let handle = ivr_pipeline(tts, stt, vad, agent)?;       // voice agent
let handle = podcast_pipeline(tts)?;                      // TTS only
let handle = transcription_pipeline(stt)?;                // STT only
let handle = enhance_pipeline()?;                         // FX transform
```

### Pipeline Metrics

`PipelineMetrics` tracks real-time latency and quality:

| Field | Description |
|-------|-------------|
| `tts_latency_ms` | TTS synthesis latency |
| `stt_latency_ms` | STT transcription latency |
| `llm_latency_ms` | Agent reasoning latency |
| `total_audio_ms` | Total audio processed |
| `vad_speech_ratio` | Speech-to-total frame ratio (0.0–1.0) |

## Agent Tools

Four tools implement `adk_core::Tool` for LLM agent integration:

| Tool | Description | Required input |
|------|-------------|----------------|
| `SpeakTool` | Synthesize text to speech | `{text, voice?, emotion?}` |
| `TranscribeTool` | Transcribe audio to text | `{audio_data (base64), sample_rate?, language?}` |
| `ApplyFxTool` | Apply a named FX chain | `{audio_data (base64), chain, sample_rate?}` |
| `GenerateMusicTool` | Generate music from prompt | `{prompt, duration_secs, genre?}` |

```rust
use adk_audio::{SpeakTool, TranscribeTool};

let speak = SpeakTool::new(tts_provider, "Rachel");
let transcribe = TranscribeTool::new(stt_provider);

let agent = LlmAgentBuilder::new("voice_assistant")
    .tool(Arc::new(speak))
    .tool(Arc::new(transcribe))
    .build()?;
```

## VAD (Voice Activity Detection)

The `VadProcessor` trait (behind `vad` feature) provides:
- `is_speech(&AudioFrame) -> bool` — binary speech detection
- `segment(&AudioFrame) -> Vec<SpeechSegment>` — identify speech segments with start/end timestamps

Used by the voice agent pipeline to gate STT inference to speech-only segments.

## Realtime Bridge

Behind the `livekit` feature, `RealtimeBridge` converts between `adk-realtime` base64-encoded PCM16 audio streams and pipeline `PipelineInput`/`PipelineOutput`:

```rust
use adk_audio::RealtimeBridge;

let bridge = RealtimeBridge::new(24000, 1); // 24kHz mono
let input_stream = bridge.from_realtime(audio_deltas);   // base64 → PipelineInput
let output_stream = bridge.to_realtime(pipeline_output);  // PipelineOutput → base64
```

## Error Handling

All operations return `AudioResult<T>` (alias for `Result<T, AudioError>`). Error variants:

| Variant | Description |
|---------|-------------|
| `Tts { provider, message }` | TTS provider error |
| `Stt { provider, message }` | STT provider error |
| `Music(String)` | Music generation error |
| `Fx(String)` | Audio processing error |
| `PipelineClosed(String)` | Pipeline misconfigured or shut down |
| `Vad(String)` | Voice activity detection error |
| `Codec(String)` | Encode/decode error |
| `ModelDownload { model_id, message }` | Model download or registry error |
| `Io(std::io::Error)` | I/O error |
| `Network(reqwest::Error)` | HTTP error (feature-gated) |

## Related Crates

- [adk-rust](https://crates.io/crates/adk-rust) — Umbrella crate
- [adk-core](https://crates.io/crates/adk-core) — `Tool` trait, `Agent` trait
- [adk-realtime](https://crates.io/crates/adk-realtime) — Real-time audio/video streaming
- [adk-tool](https://crates.io/crates/adk-tool) — Additional tool utilities

## License

See [LICENSE](../LICENSE) in the repository root.
