//! # adk-audio
//!
//! Audio intelligence and pipeline orchestration for ADK-Rust agents.
//!
//! Provides traits for text-to-speech (TTS), speech-to-text (STT), music generation, audio
//! DSP, and voice activity detection (VAD), cloud TTS and STT providers, local ONNX and Candle
//! models, and a pipeline system for voice agents, transcription, and audio processing.
//!
//! ## Features
//!
//! | Feature | Enables |
//! |---------|---------|
//! | `tts` (default) | Cloud TTS: ElevenLabs, OpenAI, Gemini, Cartesia |
//! | `stt` (default) | Cloud STT: OpenAI Whisper API, Deepgram, AssemblyAI, Gemini |
//! | `muse` | `MuseStt`: Meta Muse file and realtime transcription, over `adk-model` |
//! | `fx` | DSP processors: normalizer, resampler, noise suppressor, compressor, trimmer, pitch shifter |
//! | `mlx` | Local model loading with tokenizers and Hugging Face Hub |
//! | `onnx` | ONNX Runtime local inference; `kokoro`, `chatterbox`, `whisper-onnx`, `distil-whisper`, and `moonshine` add models |
//! | `qwen3-tts` | Qwen3-TTS on Candle |
//! | `livekit` | `RealtimeBridge` to `adk-realtime` |
//! | `desktop-audio` | Microphone capture, speaker playback, and VAD turn-taking |
//! | `all` | Every feature except `desktop-audio` |
//!
//! `VadProcessor` and `MusicProvider` are traits without a bundled implementation.
//!
//! ## Example
//!
//! ```rust
//! use adk_audio::{AudioFormat, AudioFrame, SentenceChunker, decode, encode};
//!
//! # fn main() -> adk_audio::AudioResult<()> {
//! let frame = AudioFrame::silence(16_000, 1, 250);
//! let wav = encode(&frame, AudioFormat::Wav)?;
//! assert_eq!(decode(&wav, AudioFormat::Wav)?.duration_ms, 250);
//!
//! // Sentence boundaries for streaming TTS.
//! let mut chunker = SentenceChunker::new();
//! assert_eq!(chunker.push("Hello world. How are "), vec!["Hello world."]);
//! # Ok(())
//! # }
//! ```

pub mod codec;
pub mod error;
pub mod frame;
pub mod metering;
pub mod mixer;
pub mod pipeline;
pub mod providers;
pub mod tools;
pub mod traits;

// Feature-gated modules
#[cfg(feature = "fx")]
pub mod fx;

#[cfg(feature = "mlx")]
pub mod mlx;

#[cfg(feature = "onnx")]
pub mod onnx;

#[cfg(feature = "livekit")]
pub mod bridge;

#[cfg(feature = "desktop-audio")]
pub mod desktop;

#[cfg(feature = "desktop-audio")]
pub use desktop::{
    AudioCapture, AudioDevice, AudioPlayback, AudioStream, CaptureConfig, VadConfig, VadMode,
    VadTurnManager, VoiceActivityEvent,
};

pub mod registry;

// Re-exports
pub use codec::{AudioFormat, decode, encode};
pub use error::{AudioError, AudioResult};
pub use frame::{AudioFrame, merge_frames};
pub use metering::AudioBilling;
pub use mixer::Mixer;
pub use pipeline::{
    AudioPipelineBuilder, PipelineControl, PipelineHandle, PipelineInput, PipelineMetrics,
    PipelineOutput, SentenceChunker,
};
pub use tools::{ApplyFxTool, GenerateMusicTool, SpeakTool, TranscribeTool};
pub use traits::{
    AudioProcessor, Emotion, FxChain, MusicProvider, MusicRequest, Speaker, SpeechSegment,
    SttOptions, SttProvider, Transcript, TranscriptEvent, TranscriptStream, TtsProvider,
    TtsRequest, VadProcessor, Voice, Word,
};

// Feature-gated re-exports
#[cfg(feature = "tts")]
pub use providers::tts::{
    CartesiaTts, CloudTtsConfig, ElevenLabsTts, GeminiTts, OpenAiTts, SpeakerConfig,
};

#[cfg(feature = "qwen3-tts")]
pub use providers::tts::{Qwen3TtsNativeProvider, Qwen3TtsVariant};

#[cfg(feature = "muse")]
pub use providers::stt::MuseStt;
#[cfg(feature = "stt")]
pub use providers::stt::{AssemblyAiStt, DeepgramStt, GeminiStt, WhisperApiStt};

#[cfg(feature = "fx")]
pub use fx::{
    DynamicRangeCompressor, LoudnessNormalizer, NoiseSuppressor, PitchShifter, Resampler,
    SilenceTrimmer,
};

#[cfg(feature = "livekit")]
pub use bridge::RealtimeBridge;

#[cfg(feature = "mlx")]
pub use mlx::{MlxQuantization, MlxSttConfig, MlxSttProvider, MlxTtsConfig, MlxTtsProvider};

#[cfg(feature = "onnx")]
pub use onnx::{
    OnnxExecutionProvider, OnnxModelConfig, OnnxTtsProvider, Preprocessor, PreprocessorOutput,
    TokenizerPreprocessor,
};

#[cfg(feature = "kokoro")]
pub use onnx::{KokoroPreprocessor, KokoroVoices};

#[cfg(feature = "chatterbox")]
pub use onnx::{ChatterboxConfig, ChatterboxTtsProvider, ChatterboxVariant};

#[cfg(any(feature = "whisper-onnx", feature = "distil-whisper", feature = "moonshine"))]
pub use onnx::{
    DistilWhisperVariant, MoonshineVariant, OnnxSttConfig, OnnxSttConfigBuilder, OnnxSttProvider,
    SttBackend, WhisperModelSize,
};

pub use registry::LocalModelRegistry;
