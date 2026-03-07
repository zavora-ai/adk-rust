//! # adk-audio
//!
//! Audio intelligence and pipeline orchestration for ADK-Rust agents.
//!
//! Provides unified traits for Text-to-Speech (TTS), Speech-to-Text (STT),
//! music generation, audio FX/DSP processing, and Voice Activity Detection (VAD),
//! with a composable pipeline system for building voice agent loops, podcast
//! production, transcription, and generative soundscapes.
//!
//! ## Features
//!
//! - `tts` (default) — Cloud TTS providers (ElevenLabs, OpenAI, Gemini, Cartesia)
//! - `stt` (default) — Cloud STT providers (Whisper API, Deepgram, AssemblyAI)
//! - `music` — Music generation providers
//! - `fx` — DSP processors (normalizer, resampler, noise, compressor)
//! - `vad` — Voice Activity Detection
//! - `mlx` — Apple Silicon local inference (macOS only)
//! - `onnx` — ONNX Runtime local inference (cross-platform)
//! - `opus` — Opus codec (requires cmake)
//! - `livekit` — adk-realtime bridge
//! - `all` — All non-platform features
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use adk_audio::{AudioPipelineBuilder, AudioFrame};
//!
//! let handle = AudioPipelineBuilder::new()
//!     .tts(my_tts_provider)
//!     .build_tts()?;
//! ```

pub mod codec;
pub mod error;
pub mod frame;
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

pub mod registry;

// Re-exports
pub use codec::{AudioFormat, decode, encode};
pub use error::{AudioError, AudioResult};
pub use frame::{AudioFrame, merge_frames};
pub use mixer::Mixer;
pub use pipeline::{
    AudioPipelineBuilder, PipelineControl, PipelineHandle, PipelineInput, PipelineMetrics,
    PipelineOutput, SentenceChunker,
};
pub use tools::{ApplyFxTool, GenerateMusicTool, SpeakTool, TranscribeTool};
pub use traits::{
    AudioProcessor, Emotion, FxChain, MusicProvider, MusicRequest, Speaker, SpeechSegment,
    SttOptions, SttProvider, Transcript, TtsProvider, TtsRequest, VadProcessor, Voice, Word,
};

// Feature-gated re-exports
#[cfg(feature = "tts")]
pub use providers::tts::{CartesiaTts, CloudTtsConfig, ElevenLabsTts, GeminiTts, OpenAiTts};

#[cfg(feature = "qwen3-tts")]
pub use providers::tts::{Qwen3TtsNativeProvider, Qwen3TtsVariant};

#[cfg(feature = "stt")]
pub use providers::stt::{AssemblyAiStt, DeepgramStt, WhisperApiStt};

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
