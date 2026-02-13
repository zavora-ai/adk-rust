//! Gemini Live model implementation.

use crate::audio::AudioFormat;
use crate::config::RealtimeConfig;
use crate::error::Result;
use crate::model::RealtimeModel;
use crate::session::BoxedSession;
use async_trait::async_trait;

use super::session::GeminiRealtimeSession;
use super::{DEFAULT_MODEL, GEMINI_VOICES};
use adk_gemini::GeminiLiveBackend;

/// Gemini Live model for creating realtime sessions.
///
/// # Example
///
/// ```rust,ignore
/// use adk_realtime::gemini::GeminiRealtimeModel;
/// use adk_realtime::RealtimeModel;
/// use adk_gemini::GeminiLiveBackend;
///
/// let backend = GeminiLiveBackend::Studio { api_key: "key".to_string() };
/// let model = GeminiRealtimeModel::new(backend, "models/gemini-2.0-flash-live-preview-04-09");
/// let session = model.connect(config).await?;
/// ```
#[derive(Debug, Clone)]
pub struct GeminiRealtimeModel {
    backend: GeminiLiveBackend,
    model_id: String,
}

impl GeminiRealtimeModel {
    /// Create a new Gemini Live model.
    pub fn new(backend: GeminiLiveBackend, model_id: impl Into<String>) -> Self {
        Self { backend, model_id: model_id.into() }
    }

    /// Create with the default Live model.
    pub fn with_default_model(backend: GeminiLiveBackend) -> Self {
        Self::new(backend, DEFAULT_MODEL)
    }

    /// Get the backend configuration.
    pub fn backend(&self) -> &GeminiLiveBackend {
        &self.backend
    }
}

#[async_trait]
impl RealtimeModel for GeminiRealtimeModel {
    fn provider(&self) -> &str {
        "gemini"
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn supported_input_formats(&self) -> Vec<AudioFormat> {
        vec![AudioFormat::pcm16_16khz()]
    }

    fn supported_output_formats(&self) -> Vec<AudioFormat> {
        vec![AudioFormat::pcm16_24khz()]
    }

    fn available_voices(&self) -> Vec<&str> {
        GEMINI_VOICES.to_vec()
    }

    async fn connect(&self, config: RealtimeConfig) -> Result<BoxedSession> {
        let session =
            GeminiRealtimeSession::connect(self.backend.clone(), &self.model_id, config).await?;

        Ok(Box::new(session))
    }
}

impl Default for GeminiRealtimeModel {
    fn default() -> Self {
        Self {
            backend: GeminiLiveBackend::Studio { api_key: String::new() },
            model_id: DEFAULT_MODEL.to_string(),
        }
    }
}
