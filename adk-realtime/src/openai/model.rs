//! OpenAI Realtime model implementation.

use crate::audio::AudioFormat;
use crate::config::RealtimeConfig;
use crate::error::Result;
use crate::model::RealtimeModel;
use crate::session::BoxedSession;
use async_trait::async_trait;

use super::session::OpenAIRealtimeSession;
use super::{DEFAULT_MODEL, OPENAI_REALTIME_URL, OPENAI_VOICES, OpenAITransport};

/// OpenAI Realtime model for creating realtime sessions.
///
/// # Example
///
/// ```rust,ignore
/// use adk_realtime::openai::OpenAIRealtimeModel;
/// use adk_realtime::RealtimeModel;
///
/// let model = OpenAIRealtimeModel::new("sk-...", "gpt-4o-realtime-preview-2024-12-17");
/// let session = model.connect(config).await?;
/// ```
#[derive(Debug, Clone)]
pub struct OpenAIRealtimeModel {
    api_key: String,
    model_id: String,
    base_url: Option<String>,
    transport: OpenAITransport,
}

impl OpenAIRealtimeModel {
    /// Create a new OpenAI Realtime model.
    ///
    /// # Arguments
    ///
    /// * `api_key` - Your OpenAI API key
    /// * `model_id` - The model ID (e.g., "gpt-4o-realtime-preview-2024-12-17")
    pub fn new(api_key: impl Into<String>, model_id: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model_id: model_id.into(),
            base_url: None,
            transport: OpenAITransport::default(),
        }
    }

    /// Create with the default realtime model.
    pub fn with_default_model(api_key: impl Into<String>) -> Self {
        Self::new(api_key, DEFAULT_MODEL)
    }

    /// Set a custom base URL (for proxies or alternative endpoints).
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    /// Set the transport type (WebSocket or WebRTC).
    ///
    /// By default, WebSocket transport is used. When the `openai-webrtc` feature
    /// is enabled, you can select `OpenAITransport::WebRTC` for lower-latency audio.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_realtime::openai::{OpenAIRealtimeModel, OpenAITransport};
    ///
    /// let model = OpenAIRealtimeModel::new("sk-...", "gpt-4o-realtime-preview-2024-12-17")
    ///     .with_transport(OpenAITransport::WebRTC);
    /// ```
    pub fn with_transport(mut self, transport: OpenAITransport) -> Self {
        self.transport = transport;
        self
    }

    /// Get the WebSocket URL for connection.
    pub fn websocket_url(&self) -> String {
        let base = self.base_url.as_deref().unwrap_or(OPENAI_REALTIME_URL);
        format!("{}?model={}", base, self.model_id)
    }

    /// Get the API key.
    pub fn api_key(&self) -> &str {
        &self.api_key
    }
}

#[async_trait]
impl RealtimeModel for OpenAIRealtimeModel {
    fn provider(&self) -> &str {
        "openai"
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn supported_input_formats(&self) -> Vec<AudioFormat> {
        vec![AudioFormat::pcm16_24khz(), AudioFormat::g711_ulaw(), AudioFormat::g711_alaw()]
    }

    fn supported_output_formats(&self) -> Vec<AudioFormat> {
        vec![AudioFormat::pcm16_24khz(), AudioFormat::g711_ulaw(), AudioFormat::g711_alaw()]
    }

    fn available_voices(&self) -> Vec<&str> {
        OPENAI_VOICES.to_vec()
    }

    async fn connect(&self, config: RealtimeConfig) -> Result<BoxedSession> {
        match self.transport {
            OpenAITransport::WebSocket => {
                let session =
                    OpenAIRealtimeSession::connect(&self.websocket_url(), &self.api_key, config)
                        .await?;
                Ok(Box::new(session))
            }
            #[cfg(feature = "openai-webrtc")]
            OpenAITransport::WebRTC => {
                let session = super::webrtc::OpenAIWebRTCSession::connect(
                    &self.api_key,
                    &self.model_id,
                    config,
                )
                .await?;
                Ok(Box::new(session))
            }
        }
    }
}

impl Default for OpenAIRealtimeModel {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            model_id: DEFAULT_MODEL.to_string(),
            base_url: None,
            transport: OpenAITransport::default(),
        }
    }
}
