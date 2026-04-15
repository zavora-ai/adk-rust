//! TTS provider implementations (cloud and native).

#[cfg(feature = "tts")]
mod cartesia;
#[cfg(feature = "tts")]
mod elevenlabs;
#[cfg(feature = "tts")]
mod gemini;
#[cfg(feature = "tts")]
mod openai;

#[cfg(feature = "qwen3-tts")]
pub mod qwen3_tts_native;

#[cfg(feature = "tts")]
pub use cartesia::CartesiaTts;
#[cfg(feature = "tts")]
pub use elevenlabs::ElevenLabsTts;
#[cfg(feature = "tts")]
pub use gemini::GeminiTts;
#[cfg(feature = "tts")]
pub use gemini::SpeakerConfig;
#[cfg(feature = "tts")]
pub use openai::OpenAiTts;

#[cfg(feature = "qwen3-tts")]
pub use qwen3_tts_native::{Qwen3TtsNativeProvider, Qwen3TtsVariant};

/// Shared configuration for cloud TTS providers.
#[derive(Debug, Clone)]
pub struct CloudTtsConfig {
    /// API key for authentication.
    pub api_key: String,
    /// Optional base URL override.
    pub base_url: Option<String>,
}

impl CloudTtsConfig {
    /// Create config from an API key.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self { api_key: api_key.into(), base_url: None }
    }

    /// Override the base URL.
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }
}

/// Check an HTTP response status and return an appropriate error.
#[allow(dead_code)] // Available for provider implementations
pub(crate) fn check_response(
    provider: &str,
    status: reqwest::StatusCode,
) -> Result<(), crate::error::AudioError> {
    if status.is_success() {
        Ok(())
    } else {
        Err(crate::error::AudioError::Tts {
            provider: provider.to_string(),
            message: format!("HTTP {status}"),
        })
    }
}
