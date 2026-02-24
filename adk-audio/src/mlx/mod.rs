//! MLX local inference backend for Apple Silicon.
//!
//! Provides `MlxTtsProvider` and `MlxSttProvider` for on-device TTS and STT
//! using Apple's MLX framework via `mlx-rs`. Exploits Metal GPU acceleration
//! and unified memory for zero-copy inference.
//!
//! Requires the `mlx` feature flag. Only compiles on macOS.

#[cfg(not(target_os = "macos"))]
compile_error!(
    "The `mlx` feature requires macOS with Apple Silicon (M1–M4). \
     For cross-platform local inference, use the `onnx` feature instead."
);

mod config;
mod mel;
mod stt;
mod tts;

pub use config::{MlxQuantization, MlxSttConfig, MlxTtsConfig};
pub use mel::compute_log_mel_spectrogram;
pub use stt::MlxSttProvider;
pub use tts::MlxTtsProvider;
