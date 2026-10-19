# adk-audio

Audio intelligence and pipeline orchestration for ADK-Rust agents.

[![Crates.io](https://img.shields.io/crates/v/adk-audio.svg)](https://crates.io/crates/adk-audio)
[![Documentation](https://docs.rs/adk-audio/badge.svg)](https://docs.rs/adk-audio)
[![License](https://img.shields.io/crates/l/adk-audio.svg)](../LICENSE)

`adk-audio` defines traits for text-to-speech (TTS), speech-to-text (STT), music generation,
audio DSP, and voice activity detection (VAD), ships cloud TTS and STT providers, local ONNX,
Candle, and tokenizer-based models, and composes them into pipelines for voice agents,
transcription, and audio processing.

## Installation

```toml
[dependencies]
adk-audio = "3.0.0"
```

Or through the umbrella crate:

```toml
[dependencies]
adk-rust = { version = "3.0.0", features = ["audio"] }
```

## Feature flags

| Feature | Enables | Key dependencies |
|---------|---------|------------------|
| `tts` (default) | Cloud TTS providers: ElevenLabs, OpenAI, Gemini, Cartesia | `reqwest`, `base64` |
| `stt` (default) | Cloud STT providers: OpenAI Whisper API, Deepgram, AssemblyAI, Gemini | `reqwest`, `tokio-tungstenite` |
| `fx` | DSP processors: normalizer, resampler, noise suppressor, compressor, trimmer, pitch shifter | none |
| `mlx` | Local TTS and STT model loading with tokenizers and Hugging Face Hub | `tokenizers`, `hf-hub` |
| `onnx` | ONNX Runtime local inference | `ort`, `tokenizers`, `hf-hub` |
| `kokoro` | Kokoro-82M ONNX TTS with the espeak-ng phonemizer (implies `onnx`) | `espeak-rs`, `ndarray`; needs a system espeak-ng |
| `chatterbox` | Chatterbox ONNX TTS (implies `onnx`) | |
| `whisper-onnx`, `distil-whisper`, `moonshine` | ONNX STT backends (imply `onnx`) | |
| `qwen3-tts` | Qwen3-TTS on Candle (0.6B and 1.7B) | `qwen_tts`, `candle-core`, `hf-hub` |
| `all-onnx` | `onnx`, `kokoro`, `chatterbox`, `whisper-onnx`, `distil-whisper`, `moonshine`, `qwen3-tts` | |
| `livekit` | `RealtimeBridge` between `adk-realtime` audio and pipelines | `livekit-api`, `adk-realtime` |
| `desktop-audio` | Microphone capture, speaker playback, and VAD turn-taking (implies `vad`) | `cpal` |
| `music` | Adds `reqwest` for `MusicProvider` implementations | `reqwest` |
| `vad`, `streaming` | Markers; the `VadProcessor` trait and streaming methods are always available | none |
| `all` | Every feature except `desktop-audio` | |

## Core types

### AudioFrame and codecs

`AudioFrame` is the PCM16 little-endian buffer used throughout the crate. `encode` and `decode`
convert frames to and from `Pcm16` and `Wav`; `Opus`, `Mp3`, `Flac`, and `Ogg` are defined but not
implemented — check `AudioFormat::supports_encode()` and `supports_decode()`.

```rust
use adk_audio::{AudioFormat, AudioFrame, decode, encode, merge_frames};

fn main() -> adk_audio::AudioResult<()> {
    // 100 ms of 16 kHz mono silence: 1,600 samples, 3,200 bytes.
    let frame = AudioFrame::new(vec![0u8; 3_200], 16_000, 1);
    assert_eq!(frame.duration_ms, 100);
    assert_eq!(frame.samples().len(), 1_600);

    let silence = AudioFrame::silence(16_000, 1, 500);
    let merged = merge_frames(&[frame, silence]);
    assert_eq!(merged.duration_ms, 600);

    let wav = encode(&merged, AudioFormat::Wav)?;
    let decoded = decode(&wav, AudioFormat::Wav)?;
    assert_eq!(decoded.duration_ms, 600);
    Ok(())
}
```

## Cloud providers

### TTS

Every TTS provider implements `TtsProvider` with `synthesize` and `synthesize_stream`.

| Provider | Default model | Environment variable |
|----------|---------------|----------------------|
| `ElevenLabsTts` | `eleven_multilingual_v2` | `ELEVENLABS_API_KEY` |
| `OpenAiTts` | `tts-1`; `hd()` selects `tts-1-hd` | `OPENAI_API_KEY` |
| `GeminiTts` | `gemini-3.1-flash-tts-preview` | `GEMINI_API_KEY` or `GOOGLE_API_KEY` |
| `CartesiaTts` | `sonic-3.5` | `CARTESIA_API_KEY` |

```rust
use adk_audio::{OpenAiTts, TtsProvider, TtsRequest};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tts = OpenAiTts::from_env()?;
    let request = TtsRequest {
        text: "Hello from ADK Audio!".into(),
        voice: "alloy".into(),
        speed: 1.0,
        ..Default::default()
    };
    let frame = tts.synthesize(&request).await?;
    println!("generated {} ms of audio", frame.duration_ms);
    Ok(())
}
```

`TtsRequest` also takes `language`, `pitch`, `emotion` (`Neutral`, `Happy`, `Sad`, `Angry`,
`Whisper`, `Excited`, `Calm`), and `output_format`. Each provider also accepts a `CloudTtsConfig`
with the API key and an optional base URL.

### STT

Every STT provider implements `SttProvider` with `transcribe` and `transcribe_stream`.

| Provider | Default model | Environment variable |
|----------|---------------|----------------------|
| `WhisperApiStt` | `whisper-1` | `OPENAI_API_KEY` |
| `DeepgramStt` | `nova-3`, with diarization and streaming | `DEEPGRAM_API_KEY` |
| `AssemblyAiStt` | asynchronous jobs and streaming | `ASSEMBLYAI_API_KEY` |
| `GeminiStt` | `gemini-3.7-flash` | `GEMINI_API_KEY` or `GOOGLE_API_KEY` |

```rust
use adk_audio::{AudioFrame, SttOptions, SttProvider, WhisperApiStt};

async fn transcribe(audio: &AudioFrame) -> adk_audio::AudioResult<String> {
    let stt = WhisperApiStt::from_env()?;
    let options = SttOptions { language: Some("en".into()), word_timestamps: true, ..Default::default() };
    let transcript = stt.transcribe(audio, &options).await?;
    Ok(transcript.text)
}
```

A `Transcript` carries `text`, per-`Word` timestamps and confidence, `Speaker` diarization, overall
`confidence`, and `language_detected`.

## Local inference

Models download from Hugging Face Hub into `LocalModelRegistry`'s cache
(`<cache dir>/adk-audio/models` by default; `LocalModelRegistry::new(dir)` sets another).

### ONNX TTS

```rust
use adk_audio::{OnnxTtsProvider, TtsProvider};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tts = OnnxTtsProvider::default_kokoro().await?;
    println!("{} voices", tts.voice_catalog().len());
    Ok(())
}
```

`OnnxTtsProvider::new(OnnxModelConfig, &registry)` loads another model. The provider is generic
over a `Preprocessor`: `TokenizerPreprocessor` uses a Hugging Face `tokenizer.json`, and
`KokoroPreprocessor` (`kokoro` feature) runs the espeak-ng phonemizer. Execution providers are
`OnnxExecutionProvider::Cpu`, `Cuda`, `CoreMl`, and `DirectMl`.

### ONNX STT

| Backend | Feature | Sizes |
|---------|---------|-------|
| `SttBackend::Whisper` | `whisper-onnx` | `WhisperModelSize::{Tiny, Base, Small, Medium, LargeV3Turbo}` |
| `SttBackend::DistilWhisper` | `distil-whisper` | `DistilWhisperVariant::{DistilSmallEn, DistilMediumEn, DistilLargeV3}` |
| `SttBackend::Moonshine` | `moonshine` | `MoonshineVariant::{Tiny, Base}` |

```rust
use adk_audio::{LocalModelRegistry, OnnxSttConfig, OnnxSttProvider, SttBackend, WhisperModelSize};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = OnnxSttConfig::builder()
        .stt_backend(SttBackend::Whisper)
        .model_size(WhisperModelSize::Base)
        .language("en")
        .build()?;
    let stt = OnnxSttProvider::new(config, &LocalModelRegistry::default()).await?;
    // `OnnxSttProvider::default_whisper()` loads the default Whisper model.
    let _ = stt;
    Ok(())
}
```

### MLX

`MlxTtsProvider` and `MlxSttProvider` load models with tokenizers and Hugging Face Hub, configured
by `MlxTtsConfig`, `MlxSttConfig`, and `MlxQuantization`. Metal GPU inference through `mlx-rs` is
not implemented.

```rust
use adk_audio::MlxTtsProvider;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tts = MlxTtsProvider::default_kokoro().await?;
    let _ = tts;
    Ok(())
}
```

### Qwen3-TTS

Candle-based TTS for 10 languages with predefined speakers, on CPU, Metal, or CUDA.

```rust
use adk_audio::{Qwen3TtsNativeProvider, Qwen3TtsVariant, TtsProvider, TtsRequest};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tts = Qwen3TtsNativeProvider::new(Qwen3TtsVariant::Small).await?; // 0.6B
    let request = TtsRequest {
        text: "Hello!".into(),
        voice: "vivian".into(), // or "lang:zh", or "vivian:zh"
        ..Default::default()
    };
    let frame = tts.synthesize(&request).await?;
    println!("{} ms", frame.duration_ms);
    Ok(())
}
```

Variants: `Small` (0.6B) and `Large` (1.7B). Speakers: `vivian`, `serena`, `dylan`, `eric`,
`ryan`, `aiden` (English), `uncle_fu` (Chinese), `ono_anna` (Japanese), `sohee` (Korean).

### Chatterbox

`ChatterboxTtsProvider::load(ChatterboxConfig::default())` loads
`onnx-community/chatterbox-ONNX`; `ChatterboxVariant` selects `Fp32`, `Fp16`, `Q4`, or `Q4F16`.

## DSP processors

Behind the `fx` feature; each implements `AudioProcessor`.

| Processor | Description |
|-----------|-------------|
| `LoudnessNormalizer` | EBU R128 loudness normalization, -23 LUFS by default (`with_target`) |
| `Resampler` | Sample rate conversion |
| `NoiseSuppressor` | Noise reduction below a sample threshold |
| `DynamicRangeCompressor` | Dynamic range compression |
| `SilenceTrimmer` | Leading and trailing silence removal |
| `PitchShifter` | Pitch adjustment by a factor |

`FxChain` runs processors in series:

```rust
use adk_audio::{AudioFrame, AudioProcessor, FxChain, LoudnessNormalizer, Resampler};

#[tokio::main]
async fn main() -> adk_audio::AudioResult<()> {
    let chain = FxChain::new().push(LoudnessNormalizer::with_target(-16.0)).push(Resampler::new(24_000));
    let output = chain.process(&AudioFrame::silence(16_000, 1, 200)).await?;
    assert_eq!(output.sample_rate, 24_000);
    Ok(())
}
```

## Mixer

```rust
use adk_audio::{AudioFrame, Mixer};

fn main() -> adk_audio::AudioResult<()> {
    let mut mixer = Mixer::new(24_000);
    mixer.add_track("narration", 1.0);
    mixer.add_track("music", 0.3);
    mixer.push_frame("narration", AudioFrame::silence(24_000, 1, 100));
    mixer.push_frame("music", AudioFrame::silence(24_000, 1, 100));
    let mixed = mixer.mix()?;
    println!("{} ms", mixed.duration_ms);
    Ok(())
}
```

## Pipelines

`AudioPipelineBuilder` composes providers, processors, and an agent. Each `build_*` method returns
a `PipelineHandle` with `input_tx` and `output_rx` channels, shared `metrics`, and `shutdown()`.

| Builder method | Flow | Required components |
|----------------|------|---------------------|
| `build_tts()` | Text → TTS → audio | `tts` |
| `build_stt()` | Audio → STT → transcript | `stt` |
| `build_voice_agent()` | Audio → VAD → STT → agent → TTS → audio | `tts`, `stt`, `vad`, `agent` |
| `build_transform()` | Audio → `FxChain` → audio | `pre_fx` (optional) |
| `build_music()` | Text → music provider → audio | `music` |

```rust
use std::sync::Arc;

use adk_audio::{AudioPipelineBuilder, OpenAiTts, PipelineInput, PipelineOutput};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut handle = AudioPipelineBuilder::new()
        .tts(Arc::new(OpenAiTts::from_env()?))
        .buffer_size(64) // channel buffer, 32 by default
        .build_tts()?;

    handle.input_tx.send(PipelineInput::Text("Hello there.".into())).await?;
    if let Some(output) = handle.output_rx.recv().await {
        match output {
            PipelineOutput::Audio(frame) => println!("{} ms of audio", frame.duration_ms),
            PipelineOutput::Transcript(transcript) => println!("{}", transcript.text),
            PipelineOutput::AgentText(text) => println!("{text}"),
            PipelineOutput::Metrics(metrics) => println!("{metrics:?}"),
        }
    }
    handle.shutdown();
    Ok(())
}
```

`PipelineMetrics` records `tts_latency_ms`, `stt_latency_ms`, `llm_latency_ms`, `total_audio_ms`,
and `vad_speech_ratio`. `pipeline::presets` has `ivr_pipeline(tts, stt, vad, agent)`,
`podcast_pipeline(tts)`, `transcription_pipeline(stt)`, and `enhance_pipeline()`.

### SentenceChunker

`SentenceChunker` buffers LLM tokens and emits complete sentences at `.`, `!`, `?`, `;`, and
newline boundaries, which shortens the time to the first audio in a voice agent:

```rust
use adk_audio::SentenceChunker;

fn main() {
    let mut chunker = SentenceChunker::new();
    assert_eq!(chunker.push("Hello world. How are "), vec!["Hello world."]);
    assert_eq!(chunker.push("you? Fine."), vec!["How are you?", "Fine."]);
    assert_eq!(chunker.flush(), None);
}
```

## Agent tools

Four `adk_core::Tool` implementations:

| Tool | Description | Arguments |
|------|-------------|-----------|
| `SpeakTool` | Synthesize text to speech | `{text, voice?, emotion?}` |
| `TranscribeTool` | Transcribe audio to text | `{audio_data (base64), sample_rate?, language?}` |
| `ApplyFxTool` | Apply a named FX chain | `{audio_data (base64), chain, sample_rate?}` |
| `GenerateMusicTool` | Generate music from a prompt | `{prompt, duration_secs, genre?}` |

```rust
use std::sync::Arc;

use adk_audio::{OpenAiTts, SpeakTool, TranscribeTool, WhisperApiStt};
use adk_core::Tool;

fn main() -> adk_audio::AudioResult<()> {
    let speak = SpeakTool::new(Arc::new(OpenAiTts::from_env()?), "alloy");
    let transcribe = TranscribeTool::new(Arc::new(WhisperApiStt::from_env()?));
    println!("{} and {}", speak.name(), transcribe.name());
    Ok(())
}
```

## VAD and music

`VadProcessor` (`is_speech`, `segment`) and `MusicProvider` are traits; the crate ships no
implementation of either, so the voice agent pipeline and `GenerateMusicTool` take one from the
application.

## Desktop audio

Behind `desktop-audio`: `AudioCapture` records from an input device, `AudioPlayback` plays frames
on an output device, and `VadTurnManager` turns a `VadProcessor`'s decisions into
`VoiceActivityEvent`s. `AudioCapture::list_input_devices()` and
`AudioPlayback::list_output_devices()` enumerate devices. On Linux the build needs ALSA headers
(`libasound2-dev`).

## Realtime bridge

Behind `livekit`, `RealtimeBridge` converts between `adk-realtime` base64 PCM16 audio streams and
pipeline inputs and outputs:

```rust
use std::pin::Pin;

use adk_audio::{AudioResult, PipelineInput, PipelineOutput, RealtimeBridge};
use futures::Stream;

fn bridge(
    audio_deltas: Pin<Box<dyn Stream<Item = String> + Send>>,
    pipeline_output: Pin<Box<dyn Stream<Item = PipelineOutput> + Send>>,
) -> (
    Pin<Box<dyn Stream<Item = AudioResult<PipelineInput>> + Send>>,
    Pin<Box<dyn Stream<Item = String> + Send>>,
) {
    let bridge = RealtimeBridge::new(24_000, 1); // 24 kHz mono
    (bridge.from_realtime(audio_deltas), bridge.to_realtime(pipeline_output))
}
```

## Errors

Operations return `AudioResult<T>`, an alias for `Result<T, AudioError>`:

| Variant | Description |
|---------|-------------|
| `Tts { provider, message }` | TTS provider error |
| `Stt { provider, message }` | STT provider error |
| `Music(String)` | Music generation error |
| `Fx(String)` | Audio processing error |
| `PipelineClosed(String)` | Pipeline misconfigured or shut down |
| `Vad(String)` | Voice activity detection error |
| `Codec(String)` | Encode or decode error |
| `ModelDownload { model_id, message }` | Model download or registry error |
| `Io(std::io::Error)` | I/O error |
| `Network(reqwest::Error)` | HTTP error (`tts`, `stt`, or `music`) |
| `Device(String)` | Audio device error (`desktop-audio`) |

## Related crates

- [adk-rust](https://crates.io/crates/adk-rust) — umbrella crate
- [adk-core](https://crates.io/crates/adk-core) — `Tool` and `Agent` traits
- [adk-realtime](https://crates.io/crates/adk-realtime) — real-time audio and video streaming

## License

Apache-2.0
