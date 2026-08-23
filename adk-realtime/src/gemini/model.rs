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
/// let backend = GeminiLiveBackend::studio("your-key");
/// let model = GeminiRealtimeModel::new(backend, "models/gemini-3.1-flash-live-preview");
/// let session = model.connect(config).await?;
/// ```
#[derive(Debug, Clone)]
pub struct GeminiRealtimeModel {
    backend: GeminiLiveBackend,
    model_id: String,
    schema_dialect: adk_gemini::GeminiSchemaDialect,
}

impl GeminiRealtimeModel {
    /// Create a new Gemini Live model.
    pub fn new(backend: GeminiLiveBackend, model_id: impl Into<String>) -> Self {
        Self { backend, model_id: model_id.into(), schema_dialect: Default::default() }
    }

    /// Create with the default Live model.
    pub fn with_default_model(backend: GeminiLiveBackend) -> Self {
        Self::new(backend, DEFAULT_MODEL)
    }

    /// Declare the schema dialect this model's tool schemas are reduced to.
    ///
    /// Must match the adapter that produced them. Pair
    /// [`GeminiSchemaAdapter::json_schema()`](adk_gemini::GeminiSchemaAdapter::json_schema)
    /// with [`GeminiSchemaDialect::JsonSchema`](adk_gemini::GeminiSchemaDialect::JsonSchema)
    /// so the constraints it preserved are posted under the field that accepts
    /// them. The default is the legacy OpenAPI subset, so this changes nothing
    /// unless called.
    pub fn with_schema_dialect(mut self, dialect: adk_gemini::GeminiSchemaDialect) -> Self {
        self.schema_dialect = dialect;
        self
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
        let session = GeminiRealtimeSession::connect_with_dialect(
            self.backend.clone(),
            &self.model_id,
            config,
            self.schema_dialect,
        )
        .await?;
        Ok(Box::new(session))
    }
}
