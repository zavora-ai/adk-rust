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
/// let model = OpenAIRealtimeModel::new("sk-...", "gpt-realtime-2.1");
/// let session = model.connect(config).await?;
/// ```
///
/// The API key is redacted from `Debug` output.
#[derive(Clone)]
pub struct OpenAIRealtimeModel {
    api_key: String,
    model_id: String,
    base_url: Option<String>,
    transport: OpenAITransport,
    call_id: Option<String>,
}

impl std::fmt::Debug for OpenAIRealtimeModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAIRealtimeModel")
            .field("api_key", &"[REDACTED]")
            .field("model_id", &self.model_id)
            .field("base_url", &self.base_url)
            .field("transport", &self.transport)
            .field("call_id", &self.call_id)
            .finish()
    }
}

impl OpenAIRealtimeModel {
    /// Create a new OpenAI Realtime model.
    ///
    /// # Arguments
    ///
    /// * `api_key` - Your OpenAI API key
    /// * `model_id` - The model ID (e.g., "gpt-realtime-2.1")
    pub fn new(api_key: impl Into<String>, model_id: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model_id: model_id.into(),
            base_url: None,
            transport: OpenAITransport::default(),
            call_id: None,
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
    /// let model = OpenAIRealtimeModel::new("sk-...", "gpt-realtime-2.1")
    ///     .with_transport(OpenAITransport::WebRTC);
    /// ```
    pub fn with_transport(mut self, transport: OpenAITransport) -> Self {
        self.transport = transport;
        self
    }

    /// Attaches every session this model connects to the SIP call `call_id`, instead of
    /// creating a new session.
    ///
    /// OpenAI answers the call with the session configured when the call was accepted
    /// (`POST /v1/realtime/calls/{call_id}/accept`); the WebSocket joins that session at
    /// `wss://api.openai.com/v1/realtime?call_id={call_id}`, without a `model` parameter, and
    /// its session updates leave the audio format to the call. The model ID still prices the
    /// session's usage. The `openai::sip` module (feature `openai-sip`) accepts calls.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_realtime::openai::OpenAIRealtimeModel;
    ///
    /// let model = OpenAIRealtimeModel::new("sk-...", "gpt-realtime-2.1").with_call_id("rtc_123");
    /// assert_eq!(model.websocket_url(), "wss://api.openai.com/v1/realtime?call_id=rtc_123");
    /// ```
    pub fn with_call_id(mut self, call_id: impl Into<String>) -> Self {
        self.call_id = Some(call_id.into());
        self
    }

    /// The SIP call this model attaches to, if any.
    pub fn call_id(&self) -> Option<&str> {
        self.call_id.as_deref()
    }

    /// Get the WebSocket URL for connection.
    pub fn websocket_url(&self) -> String {
        let base = self.base_url.as_deref().unwrap_or(OPENAI_REALTIME_URL);
        match &self.call_id {
            Some(call_id) => {
                let call_id: String =
                    url::form_urlencoded::byte_serialize(call_id.as_bytes()).collect();
                format!("{base}?call_id={call_id}")
            }
            None => format!("{}?model={}", base, self.model_id),
        }
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
                let url = self.websocket_url();
                let session = match self.call_id {
                    Some(_) => {
                        OpenAIRealtimeSession::connect_to_call(&url, &self.api_key, config).await?
                    }
                    None => OpenAIRealtimeSession::connect(&url, &self.api_key, config).await?,
                };
                let handler = crate::openai::protocol::OpenAIProtocolHandler::new(session);
                Ok(Box::new(handler))
            }
            #[cfg(feature = "openai-webrtc")]
            OpenAITransport::WebRTC => {
                if self.call_id.is_some() {
                    return Err(crate::error::RealtimeError::config(
                        "a SIP call is attached over WebSocket; use OpenAITransport::WebSocket",
                    ));
                }
                let session = super::webrtc::OpenAIWebRTCSession::connect(
                    &self.api_key,
                    &self.model_id,
                    config,
                )
                .await?;
                let handler = crate::openai::protocol::OpenAIProtocolHandler::new(session);
                Ok(Box::new(handler))
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
            call_id: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_redacts_the_api_key() {
        let model = OpenAIRealtimeModel::new("sk-live-secret-123", "gpt-realtime-2.1");
        let rendered = format!("{model:?}");
        assert!(!rendered.contains("sk-live-secret-123"), "{rendered}");
        assert!(rendered.contains("[REDACTED]"), "{rendered}");
        assert!(rendered.contains("gpt-realtime-2.1"), "{rendered}");
    }
}
