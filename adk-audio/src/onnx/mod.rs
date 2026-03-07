//! ONNX Runtime local inference backend (cross-platform).
//!
//! Provides `OnnxTtsProvider` for on-device TTS inference via ONNX Runtime,
//! supporting CUDA, DirectML, CoreML, and CPU execution providers.
//!
//! The provider is generic over a [`Preprocessor`] trait, allowing different
//! ONNX TTS models to plug in their own text→tokens pipeline:
//!
//! - [`TokenizerPreprocessor`] — default, uses HuggingFace `tokenizer.json`
//! - [`KokoroPreprocessor`] — espeak-ng phonemizer for Kokoro-82M (requires `kokoro` feature)
//!
//! Requires the `onnx` feature flag.

mod execution_provider;
mod preprocessor;
mod tts;
mod voice;

pub use execution_provider::OnnxExecutionProvider;
pub use preprocessor::{Preprocessor, PreprocessorOutput, TokenizerPreprocessor};
pub use tts::{OnnxModelConfig, OnnxTtsProvider};

#[cfg(feature = "kokoro")]
pub use preprocessor::{KokoroPreprocessor, KokoroVoices};

#[cfg(feature = "chatterbox")]
mod chatterbox;

#[cfg(feature = "chatterbox")]
pub use chatterbox::{ChatterboxConfig, ChatterboxTtsProvider, ChatterboxVariant};

#[cfg(any(feature = "whisper-onnx", feature = "distil-whisper", feature = "moonshine"))]
mod stt;

#[cfg(any(feature = "whisper-onnx", feature = "distil-whisper", feature = "moonshine"))]
pub use stt::{
    DistilWhisperVariant, MoonshineVariant, OnnxSttConfig, OnnxSttConfigBuilder, OnnxSttProvider,
    SttBackend, WhisperModelSize,
};

// qwen3-tts is now a native provider (not ONNX-based), see providers/tts/qwen3_tts_native.rs
