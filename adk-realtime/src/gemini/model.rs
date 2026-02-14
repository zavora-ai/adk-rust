//! Gemini Live model implementation.

use crate::audio::AudioFormat;
use crate::config::RealtimeConfig;
use crate::error::Result;
use crate::model::RealtimeModel;
use crate::session::BoxedSession;
use async_trait::async_trait;

use super::session::{GeminiLiveBackend, GeminiRealtimeSession};
use super::{DEFAULT_MODEL, GEMINI_VOICES};

/// Gemini Live model for creating realtime sessions.
///
/// # Example
///
/// ```rust,ignore
/// use adk_realtime::gemini::{GeminiRealtimeModel, GeminiLiveBackend};
/// use adk_realtime::RealtimeModel;
///
/// let backend = GeminiLiveBackend::Studio { api_key: "your-key".into() };
/// let model = GeminiRealtimeModel::new(backend, "models/gemini-live-2.5-flash-native-audio");
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
