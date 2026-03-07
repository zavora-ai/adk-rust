//! MLX local inference backend for Apple Silicon.
//!
//! Provides `MlxTtsProvider` and `MlxSttProvider` for on-device TTS and STT.
//! Currently uses `tokenizers` + `hf-hub` for model loading and tokenization.
//! Full Metal GPU inference via `mlx-rs` is planned for a future release.
//!
//! Requires the `mlx` feature flag.

mod config;
mod mel;
mod stt;
mod tts;

pub use config::{MlxQuantization, MlxSttConfig, MlxTtsConfig};
pub use mel::compute_log_mel_spectrogram;
pub use stt::MlxSttProvider;
pub use tts::MlxTtsProvider;
