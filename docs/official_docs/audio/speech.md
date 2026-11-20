# Speech with adk-audio

`adk-audio` turns speech into text (STT) and text into speech (TTS) behind two traits, with
cloud providers and on-device models on ONNX Runtime and Candle. Every provider takes and
returns the same `AudioFrame`: 16-bit little-endian PCM with its sample rate and channel count.

| Need | Use |
|------|-----|
| Transcribe a recording or a live stream, or speak a reply | This page: `SttProvider`, `TtsProvider` |
| A two-way spoken conversation with a model (OpenAI Realtime, Gemini Live) | [Realtime & Multimodal](../realtime/index.md) (`adk-realtime`) |
| The microphone, the speaker and turn-taking on a desktop | [Desktop Audio](../agents/desktop-audio.md) (`desktop-audio`) |
| Pipelines, DSP effects, the mixer and every type | The [crate README](https://github.com/zavora-ai/adk-rust/blob/main/adk-audio/README.md) |

## Install

```toml
[dependencies]
adk-audio = "3.0.0"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

The default features, `tts` and `stt`, compile every cloud provider. On-device models are
opt-in:

```toml
[dependencies]
adk-audio = { version = "3.0.0", features = ["whisper-onnx", "chatterbox"] }
```

| Feature | Adds |
|---------|------|
| `stt` (default) | Cloud STT: `WhisperApiStt`, `DeepgramStt`, `AssemblyAiStt`, `GeminiStt` |
| `tts` (default) | Cloud TTS: `ElevenLabsTts`, `OpenAiTts`, `GeminiTts`, `CartesiaTts` |
| `muse` | `MuseStt`: Meta Muse file and realtime transcription, over `adk-model` (implies `stt`) |
| `whisper-onnx`, `distil-whisper`, `moonshine` | `OnnxSttProvider` with the Whisper, Distil-Whisper and Moonshine recognisers on ONNX Runtime (each implies `onnx`) |
| `kokoro` | `KokoroPreprocessor` and `KokoroVoices` for Kokoro-82M on `OnnxTtsProvider`; needs a system espeak-ng (implies `onnx`) |
| `chatterbox` | `ChatterboxTtsProvider`: voice cloning from a reference WAV (implies `onnx`) |
| `qwen3-tts` | `Qwen3TtsNativeProvider`: Qwen3-TTS 0.6B and 1.7B on Candle |
| `onnx` | ONNX Runtime and `OnnxTtsProvider`; `ort` downloads the onnxruntime binary at build time |
| `onnx-cuda`, `onnx-coreml` | The CUDA and CoreML execution providers (each implies `onnx`) |
| `all-onnx` | `onnx`, `kokoro`, `chatterbox`, `whisper-onnx`, `distil-whisper`, `moonshine` and `qwen3-tts` |
| `mlx` | `MlxSttProvider` and `MlxTtsProvider` model loading; inference is not implemented |
| `fx` | DSP processors: loudness normalizer, resampler, noise suppressor, compressor, silence trimmer, pitch shifter |
| `music` | `reqwest` for your own `MusicProvider` |
| `livekit` | `RealtimeBridge` between `adk-realtime` audio and `adk-audio` pipelines |
| `desktop-audio` | Microphone capture, speaker playback and VAD turn-taking ([Desktop Audio](../agents/desktop-audio.md)) |
| `all` | Every feature except `desktop-audio`, `onnx-cuda` and `onnx-coreml` |

Through the umbrella crate, `audio` enables `adk-audio` with its defaults; `audio-onnx` adds
`whisper-onnx`, `distil-whisper`, `moonshine` and `chatterbox`; `kokoro`, `qwen3-tts` and
`all-onnx` keep their names; `audio-all` enables `all`.

```toml
[dependencies]
adk-rust = { version = "3.0.0", features = ["audio", "audio-onnx"] }
```

## The traits

| Trait | Methods | Implementations |
|-------|---------|-----------------|
| `SttProvider` | `transcribe(&AudioFrame, &SttOptions) -> Transcript`; `transcribe_stream(stream of AudioFrame, &SttOptions) -> TranscriptStream`; `billing()` | The STT providers below |
| `TtsProvider` | `synthesize(&TtsRequest) -> AudioFrame`; `synthesize_stream(&TtsRequest)` → a stream of `AudioFrame`s; `voice_catalog()`; `billing()` | The TTS providers below |
| `AudioProcessor` | `process(&AudioFrame) -> AudioFrame` | The `fx` processors and `FxChain` |
| `VadProcessor` | `is_speech(&AudioFrame) -> bool`; `segment(&AudioFrame)` | None: the application supplies the detector |
| `MusicProvider` | `generate(&MusicRequest) -> AudioFrame`; `supported_genres()` | None: the application supplies the provider |

Every method that can fail returns `AudioResult<T>`; a provider's failure is
`AudioError::Stt { provider, message }` or `AudioError::Tts { provider, message }`, and a model
that cannot be fetched is `AudioError::ModelDownload { model_id, message }`.

## Speech to text

### Providers

| Provider | Feature | Model | `transcribe_stream` | Credentials |
|----------|---------|-------|---------------------|-------------|
| `GeminiStt` | `stt` | `gemini-3.8-flash`; `with_model` sets another | `AudioError::Stt`; live audio goes through the Gemini Live API in `adk-realtime` | `GEMINI_API_KEY` or `GOOGLE_API_KEY` |
| `DeepgramStt` | `stt` | `nova-3`; on the stream, `SttOptions::model_hint` sets another | A WebSocket that takes 16 kHz mono PCM16: `Partial` for interim results, `Final` per finalised segment, `SpeechStarted` and `SpeechEnded` from voice activity, `Usage` from the closing metadata | `DEEPGRAM_API_KEY` |
| `MuseStt` | `muse` | `muse-voice-transcribe-1.0` | Meta's realtime WebSocket: `Started`, cumulative `Partial`s, one `Final` per turn, `SpeechStarted` and `SpeechEnded`, `Speaker` with diarization, `Progress`, `Usage`; a session lasts at most 60 minutes | `META_API_KEY` or `MODEL_API_KEY` |
| `WhisperApiStt` | `stt` | `whisper-1` | `AudioError::Stt` | `OPENAI_API_KEY` |
| `AssemblyAiStt` | `stt` | An upload and a polled transcription job | `AudioError::Stt` | `ASSEMBLYAI_API_KEY` |
| `OnnxSttProvider` | `whisper-onnx`, `distil-whisper`, `moonshine` | Whisper base by default ([On the device](#on-the-device)) | One `Final` per 30-second segment, numbered from 0 | None |
| `MlxSttProvider` | `mlx` | `mlx-community/whisper-large-v3-turbo` | `AudioError::Stt` | None; `transcribe` also returns `AudioError::Stt`, since MLX inference is not implemented |

Each cloud provider has `from_env()`, which reads the variables above and returns
`AudioError::Stt` naming the missing one. `MuseStt::transcribe_stream` and its 60-minute limit
are described under [Muse realtime transcription](../models/providers.md#realtime-transcription).

`SttOptions` is built from `SttOptions::default()` and its `with_*` methods; each provider reads
the options it supports.

| Option | Method | Read by |
|--------|--------|---------|
| Language (BCP-47) | `with_language` | Gemini (in its prompt), Deepgram, Whisper API, AssemblyAI, Muse, ONNX Whisper and Distil-Whisper (over the config's `language`) |
| Diarization | `with_diarize` | Deepgram, AssemblyAI, Muse |
| Word timestamps | `with_word_timestamps` | Deepgram, Whisper API |
| Smart formatting | `with_smart_format` | Deepgram |
| Model | `with_model_hint` | Deepgram's stream |
| Domain vocabulary | `with_keywords` | Muse |
| Likely languages | `with_language_bias` | Muse |

### Transcribe a recording

```rust
use adk_audio::{AudioFormat, GeminiStt, SttOptions, SttProvider, decode};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let audio = decode(&std::fs::read("meeting.wav")?, AudioFormat::Wav)?;

    let stt = GeminiStt::from_env()?;
    let options = SttOptions::default().with_language("en-US");
    let transcript = stt.transcribe(&audio, &options).await?;

    println!("{}", transcript.text);
    Ok(())
}
```

A `Transcript` carries `text`, per-`Word` timings and confidence, the `Speaker`s found by
diarization, an overall `confidence` and `language_detected`.

### Live transcription

`transcribe_stream` takes a stream of `AudioFrame`s and returns a `TranscriptStream` of
`AudioResult<TranscriptEvent>`.

| Event | Meaning |
|-------|---------|
| `Started { session_id }` | The provider accepted the session |
| `SpeechStarted { turn_id, offset_ms }` | Speech began; later events with the same `turn_id` belong to it |
| `Partial { turn_id, text }` | The current hypothesis for the turn, in full: replace the text shown, never append |
| `Final { turn_id, transcript }` | Text the provider will not revise |
| `SpeechEnded { turn_id, offset_ms }` | Speech stopped |
| `Speaker { label, offset_ms }` | The speaker changed (diarization) |
| `Progress { processed_ms }` | Audio the provider has processed so far |
| `Usage { audio_ms }` | The audio the session is billed for |

`TranscriptEvent` is `#[non_exhaustive]`, so a `match` on it keeps a wildcard arm. Dropping the
stream stops the provider's work for it: `DeepgramStt` stops sending and closes its socket, and
the caller's audio stream is released.

```rust
use adk_audio::{
    AudioFormat, AudioFrame, DeepgramStt, SttOptions, SttProvider, TranscriptEvent, decode,
};
use futures::{StreamExt, stream};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Deepgram's stream takes 16 kHz mono PCM16; 100 ms frames stand in for a microphone.
    let call = decode(&std::fs::read("call-16k-mono.wav")?, AudioFormat::Wav)?;
    let frames: Vec<AudioFrame> =
        call.data.chunks(3_200).map(|chunk| AudioFrame::new(chunk.to_vec(), 16_000, 1)).collect();

    let stt = DeepgramStt::from_env()?;
    let options = SttOptions::default().with_language("en").with_diarize(true);
    let mut events = stt.transcribe_stream(Box::pin(stream::iter(frames)), &options).await?;

    let mut shown = String::new();
    while let Some(event) = events.next().await {
        match event? {
            TranscriptEvent::Partial { text, .. } => shown = text,
            TranscriptEvent::Final { transcript, .. } => {
                println!("{}", transcript.text);
                shown.clear();
            }
            TranscriptEvent::Speaker { label, .. } => println!("[{label}]"),
            TranscriptEvent::Usage { audio_ms } => println!("billed for {audio_ms} ms"),
            _ => {}
        }
    }
    Ok(())
}
```

### Moving from 2.x

In 2.x, `transcribe_stream` yielded `Transcript`s, and nothing told an interim result from a
final one. In 3.0 it yields `TranscriptEvent`s, has a default implementation that returns
`AudioError::Stt`, and `SttOptions` is `#[non_exhaustive]`.

| 2.x | 3.0 |
|-----|-----|
| A stream item `Ok(transcript)` for interim text | `Ok(TranscriptEvent::Partial { text, .. })` |
| A stream item `Ok(transcript)` for final text | `Ok(TranscriptEvent::Final { transcript, .. })` |
| — | `Started`, `SpeechStarted`, `SpeechEnded`, `Speaker`, `Progress`, `Usage` |
| An implementation returns `Pin<Box<dyn Stream<Item = AudioResult<Transcript>> + Send>>` | It returns `TranscriptStream` |
| A provider without streaming implements `transcribe_stream` to fail or to return an empty stream | It leaves `transcribe_stream` out; the default returns `AudioError::Stt` |
| `GeminiStt` and `WhisperApiStt` streams end at once with no items | They return `AudioError::Stt`, which says to call `transcribe` with the buffered audio |
| `SttOptions { language: Some(..), ..Default::default() }` | `SttOptions::default().with_language(..)` |

The [2.x → 3.0 migration guide](../migration/2.x-to-3.0.md#adk-audio) lists the change with the
rest of the release.

### Write a provider

A provider implements `transcribe`, and `transcribe_stream` when it can stream. This one hears
the same words in any audio, the way a test double does, and runs without a key:

```rust
use std::pin::Pin;

use adk_audio::{
    AudioFrame, AudioResult, SttOptions, SttProvider, Transcript, TranscriptEvent,
    TranscriptStream,
};
use async_trait::async_trait;
use futures::{Stream, StreamExt, stream};

struct ScriptedStt {
    words: String,
}

#[async_trait]
impl SttProvider for ScriptedStt {
    async fn transcribe(&self, _audio: &AudioFrame, _opts: &SttOptions) -> AudioResult<Transcript> {
        Ok(Transcript { text: self.words.clone(), confidence: 1.0, ..Default::default() })
    }

    async fn transcribe_stream(
        &self,
        audio: Pin<Box<dyn Stream<Item = AudioFrame> + Send>>,
        _opts: &SttOptions,
    ) -> AudioResult<TranscriptStream> {
        let words = self.words.clone();
        let heard = audio.fold(0_u64, |ms, frame| async move { ms + u64::from(frame.duration_ms) });
        let events = stream::once(heard).flat_map(move |audio_ms| {
            let first = words.split(' ').next().unwrap_or_default().to_string();
            let transcript = Transcript { text: words.clone(), ..Default::default() };
            stream::iter([
                Ok(TranscriptEvent::Partial { turn_id: Some(0), text: first }),
                Ok(TranscriptEvent::Final { turn_id: Some(0), transcript }),
                Ok(TranscriptEvent::Usage { audio_ms }),
            ])
        });
        Ok(Box::pin(events))
    }
}

/// A provider without streaming: the trait's default `transcribe_stream` refuses.
struct BatchOnly;

#[async_trait]
impl SttProvider for BatchOnly {
    async fn transcribe(&self, _audio: &AudioFrame, _opts: &SttOptions) -> AudioResult<Transcript> {
        Ok(Transcript::default())
    }
}

#[tokio::main]
async fn main() -> AudioResult<()> {
    let stt = ScriptedStt { words: "pay the invoice".into() };
    let audio = || Box::pin(stream::iter(vec![AudioFrame::silence(16_000, 1, 500); 4]));

    let mut events = stt.transcribe_stream(audio(), &SttOptions::default()).await?;
    while let Some(event) = events.next().await {
        println!("{:?}", event?);
    }

    let refused = BatchOnly.transcribe_stream(audio(), &SttOptions::default()).await;
    assert!(refused.is_err());
    Ok(())
}
```

It prints the `Partial` with `"pay"`, the `Final` with `"pay the invoice"`, and
`Usage { audio_ms: 2000 }`.

## Text to speech

| Provider | Feature | Model | Voices | `synthesize_stream` | Credentials |
|----------|---------|-------|--------|---------------------|-------------|
| `GeminiTts` | `tts` | `gemini-3.1-flash-tts-preview`; `with_model` sets another | 30 named voices (`Kore`, `Puck`, …); `with_speakers` voices a dialogue between named speakers | One frame | `GEMINI_API_KEY` or `GOOGLE_API_KEY` |
| `ElevenLabsTts` | `tts` | `eleven_multilingual_v2` | Four catalog voices; any ElevenLabs voice id | Chunks as they arrive | `ELEVENLABS_API_KEY` |
| `OpenAiTts` | `tts` | `tts-1`; `hd()` selects `tts-1-hd` | `alloy`, `echo`, `fable`, `onyx`, `nova`, `shimmer` | One frame | `OPENAI_API_KEY` |
| `CartesiaTts` | `tts` | `sonic-3.5` | Two catalog voices; any Cartesia voice id | One frame | `CARTESIA_API_KEY` |
| `OnnxTtsProvider` + `KokoroPreprocessor` | `kokoro` | Kokoro-82M from a local model directory | The voices in Kokoro's `.npz` voices file | The finished audio in 100 ms frames | None |
| `ChatterboxTtsProvider` | `chatterbox` | `onnx-community/chatterbox-ONNX`; `ChatterboxVariant` `Fp32` (default), `Fp16`, `Q4`, `Q4F16` | Cloned from `ChatterboxConfig::reference_wav`, or the repository's `default_voice.wav` | The finished audio in 100 ms frames | None |
| `Qwen3TtsNativeProvider` | `qwen3-tts` | `Qwen3TtsVariant::Small` (0.6B) or `Large` (1.7B) | `vivian`, `serena`, `dylan`, `eric`, `ryan`, `aiden`, `uncle_fu`, `ono_anna`, `sohee`; 10 languages | The finished audio in 100 ms frames | None |
| `MlxTtsProvider` | `mlx` | `mlx-community/Kokoro-82M-bf16` | — | — | None; `synthesize` returns `AudioError::Tts`, since MLX inference is not implemented |

The cloud providers return 24 kHz mono PCM16. Each has `from_env()` and `new(CloudTtsConfig)`;
`CloudTtsConfig::with_base_url` points one at another endpoint.

```rust
use adk_audio::{AudioFormat, GeminiTts, TtsProvider, TtsRequest, encode};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tts = GeminiTts::from_env()?;
    let request = TtsRequest {
        text: "Your invoice is paid.".into(),
        voice: "Kore".into(),
        ..Default::default()
    };
    let frame = tts.synthesize(&request).await?;

    std::fs::write("paid.wav", encode(&frame, AudioFormat::Wav)?)?;
    println!("{} ms at {} Hz", frame.duration_ms, frame.sample_rate);
    Ok(())
}
```

`TtsRequest` also takes `language`, `speed` (1.0 by default), `pitch` and an `emotion` hint:
`Neutral`, `Happy`, `Sad`, `Angry`, `Whisper`, `Excited` or `Calm`.

## On the device

The on-device recognisers and voices need no key, and their audio never leaves the machine.

### Recognisers

`OnnxSttConfig::builder()` picks the recogniser; `OnnxSttProvider::new(config, &registry)` loads
it. Audio of any rate and channel count is downmixed to mono and resampled to 16 kHz.

| Backend | Feature | Variants and model |
|---------|---------|--------------------|
| `SttBackend::Whisper` (default) | `whisper-onnx` | `WhisperModelSize::Tiny`, `Base` (default), `Small`, `Medium`, `LargeV3Turbo` → `onnx-community/whisper-<size>` |
| `SttBackend::DistilWhisper` | `distil-whisper` | `DistilWhisperVariant::DistilSmallEn` (default), `DistilMediumEn`, `DistilLargeV3` → `distil-whisper/distil-small.en`, `distil-medium.en`, `distil-large-v3` |
| `SttBackend::Moonshine` | `moonshine` | `MoonshineVariant::Tiny` (default), `Base` → `usefulsensors/moonshine-tiny-onnx`, `moonshine-base-onnx` |

The builder also sets `beam_size` (1 to 10, 1 by default), `temperature` (0.0 by default),
`language` (detected when unset) and `execution_provider`. `OnnxSttProvider::default_whisper()`
loads Whisper base.

```rust
use adk_audio::{
    AudioFormat, LocalModelRegistry, OnnxSttConfig, OnnxSttProvider, SttBackend, SttOptions,
    SttProvider, WhisperModelSize, decode,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = OnnxSttConfig::builder()
        .stt_backend(SttBackend::Whisper)
        .model_size(WhisperModelSize::Small)
        .language("en")
        .build()?;
    assert_eq!(config.model_id(), "onnx-community/whisper-small");

    let stt = OnnxSttProvider::new(config, &LocalModelRegistry::default()).await?;
    let audio = decode(&std::fs::read("note.wav")?, AudioFormat::Wav)?;
    let transcript = stt.transcribe(&audio, &SttOptions::default()).await?;

    println!("{}", transcript.text);
    Ok(())
}
```

The example needs the `whisper-onnx` feature.

### Voices

| Voice | What it needs |
|-------|---------------|
| Kokoro-82M (`kokoro`) | espeak-ng installed (`brew install espeak-ng pkg-config`; `apt-get install espeak-ng libespeak-ng-dev`), the model directory on disk, and Kokoro's voices file: `KokoroPreprocessor::new(voices_path, "en-us")` passed to `OnnxTtsProvider::with_preprocessor(config, model_dir, preprocessor)`, with `OnnxModelConfig::onnx_filename` naming the model's file |
| Chatterbox (`chatterbox`) | `ChatterboxTtsProvider::load(ChatterboxConfig::default())`; set `reference_wav` to clone a voice and `exaggeration` (0.0–1.0, 0.5 by default) for expressiveness; it runs on the CPU unless `execution_provider` says otherwise |
| Qwen3-TTS (`qwen3-tts`) | `Qwen3TtsNativeProvider::new(variant)`, or `from_dir` for a model already on disk; on macOS it runs on Metal when the build's `candle-core` has Metal support and on the CPU otherwise; elsewhere on the CPU. The request's `voice` is a speaker (`vivian`), a language (`lang:zh`) or both (`vivian:zh`) |
| Another ONNX model | `OnnxTtsProvider::new(OnnxModelConfig, &registry)` with the model's Hugging Face id; its `tokenizer.json` drives `TokenizerPreprocessor` |

### Model downloads

| Provider | Where the weights come from |
|----------|-----------------------------|
| `OnnxSttProvider`, `OnnxTtsProvider::new`, `Qwen3TtsNativeProvider::new`, the MLX providers | `LocalModelRegistry`: a directory `<org>--<name>` under its cache (`~/.cache/adk-audio/models` by default; `LocalModelRegistry::new(dir)` sets another) is used as is; otherwise every file of the Hugging Face repository is downloaded on first use into the Hugging Face cache |
| `ChatterboxTtsProvider` | The model, tokenizer and `default_voice.wav` files from its repository, downloaded on first use into the Hugging Face cache |
| `OnnxTtsProvider::with_preprocessor`, `Qwen3TtsNativeProvider::from_dir` | The directory you pass; nothing is downloaded |

The first call that loads a model can therefore download hundreds of megabytes. A download that
fails is `AudioError::ModelDownload`.

### Execution providers

`OnnxExecutionProvider` is `Cpu`, `Cuda`, `CoreMl` or `DirectMl`; `auto_detect()` returns
`CoreMl` on macOS and `Cpu` elsewhere. `Cuda` runs only with the `onnx-cuda` feature and
`CoreMl` only with `onnx-coreml`; without its feature, and for `DirectMl`, a provider logs a
warning and the session runs on the CPU.

## Speech as agent tools

`TranscribeTool` and `SpeakTool` are `adk_core::Tool`s over any provider, for an agent that
hears or speaks.

| Tool | Name | Arguments | Returns |
|------|------|-----------|---------|
| `TranscribeTool::new(stt)` | `transcribe` | `audio_data` (base64 PCM16), `sample_rate` (16000 by default), `language` | `text`, `confidence` |
| `SpeakTool::new(tts, default_voice)` | `speak` | `text`, `voice`, `emotion` | `duration_ms`, `sample_rate` |

```rust
use std::sync::Arc;

use adk_audio::{GeminiStt, GeminiTts, SpeakTool, TranscribeTool};
use adk_core::Tool;

fn main() -> adk_audio::AudioResult<()> {
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(TranscribeTool::new(Arc::new(GeminiStt::from_env()?))),
        Arc::new(SpeakTool::new(Arc::new(GeminiTts::from_env()?), "Kore")),
    ];
    for tool in &tools {
        println!("{}", tool.name());
    }
    Ok(())
}
```

A cloud provider's `billing()` names the vendor and model its calls are billed under, and the
two tools meter each call against the run: `TranscribeTool` in `audioMillis`, `SpeakTool` in
`characters`. These providers carry no per-unit price, so under a run budget's cost cap or a
spend-ledger limit their calls are refused before they are sent; `RunBudget::allow_unpriced_models()`
lets a budget's cost cap admit them. See
[Calls billed in units](../security/spend-ledger.md#calls-billed-in-units). `MuseStt` returns
`None` from `billing()`, since the Muse clients meter their own calls at the catalog's price, and
so do the local models, which are not metered.

## Related

- [Desktop Audio](../agents/desktop-audio.md) — the microphone, the speaker and VAD turn-taking
- [Realtime & Multimodal](../realtime/index.md) — spoken conversations with OpenAI Realtime and Gemini Live
- [Telephony](../realtime/telephony.md) — phone calls on a realtime session
- [Model providers: Meta Muse](../models/providers.md#realtime-transcription) — the Muse transcription clients under `MuseStt`
- [The crate README](https://github.com/zavora-ai/adk-rust/blob/main/adk-audio/README.md) — pipelines, DSP, the mixer and the realtime bridge
