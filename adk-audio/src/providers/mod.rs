//! Cloud and native provider implementations.

#[cfg(any(feature = "tts", feature = "qwen3-tts"))]
pub mod tts;

#[cfg(feature = "stt")]
pub mod stt;
