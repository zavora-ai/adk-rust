//! ONNX Runtime local inference backend (cross-platform).
//!
//! Provides `OnnxTtsProvider` for on-device TTS inference via ONNX Runtime,
//! supporting CUDA, DirectML, CoreML, and CPU execution providers.
//!
//! Requires the `onnx` feature flag.

mod execution_provider;
mod tts;
mod voice;

pub use execution_provider::OnnxExecutionProvider;
pub use tts::{OnnxModelConfig, OnnxTtsProvider};
