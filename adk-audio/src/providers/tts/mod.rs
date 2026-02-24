//! Cloud TTS provider implementations.

mod cartesia;
mod elevenlabs;
mod gemini;
mod openai;

pub use cartesia::CartesiaTts;
pub use elevenlabs::ElevenLabsTts;
pub use gemini::GeminiTts;
pub use openai::OpenAiTts;

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
