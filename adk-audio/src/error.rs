//! Error types for the adk-audio crate.

use thiserror::Error;

/// Errors produced by audio subsystems.
#[derive(Debug, Error)]
pub enum AudioError {
    /// Text-to-speech provider error.
    #[error("TTS error ({provider}): {message}")]
    Tts {
        /// Provider name (e.g. "elevenlabs", "openai").
        provider: String,
        /// Actionable error message.
        message: String,
    },

    /// Speech-to-text provider error.
    #[error("STT error ({provider}): {message}")]
    Stt {
        /// Provider name (e.g. "whisper", "deepgram").
        provider: String,
        /// Actionable error message.
        message: String,
    },

    /// Music generation error.
    #[error("Music generation error: {0}")]
    Music(String),

    /// Audio processing / FX error.
    #[error("Audio processing error: {0}")]
    Fx(String),

    /// Pipeline is closed or misconfigured.
    #[error("Pipeline closed: {0}")]
    PipelineClosed(String),

    /// Voice activity detection error.
    #[error("VAD error: {0}")]
    Vad(String),

    /// Codec encode/decode error.
    #[error("Codec error: {0}")]
    Codec(String),

    /// Model download or registry error.
    #[error("Model download failed for '{model_id}': {message}")]
    ModelDownload {
        /// HuggingFace model identifier.
        model_id: String,
        /// Actionable error message.
        message: String,
    },

    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Network / HTTP error.
    #[cfg(any(feature = "tts", feature = "stt", feature = "music"))]
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),

    /// Desktop audio device error (capture, playback, enumeration).
    #[cfg(feature = "desktop-audio")]
    #[error("Device error: {0}")]
    Device(String),
}

/// Convenience result type for audio operations.
pub type AudioResult<T> = Result<T, AudioError>;
