//! Gemini Live session implementation.
//!
//! Manages a WebSocket connection to Google's Gemini Live API with support
//! for both AI Studio (API key) and Vertex AI (OAuth/ADC) backends.

use crate::audio::AudioChunk;
use crate::config::{RealtimeConfig, ToolDefinition};
use crate::error::{RealtimeError, Result};
use crate::events::{ClientEvent, ServerEvent, ToolResponse};
use crate::session::{ContextMutationOutcome, RealtimeSession};
use async_trait::async_trait;
use base64::Engine;
use futures::stream::Stream;
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{connect_async, tungstenite::Message};

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsSink = futures::stream::SplitSink<WsStream, Message>;
type WsSource = futures::stream::SplitStream<WsStream>;

/// Backend configuration for Gemini Live connections.
///
/// Determines how to authenticate and which endpoint to connect to.
#[derive(Debug, Clone)]
pub enum GeminiLiveBackend {
    /// AI Studio with API key authentication.
    Studio { api_key: String },

    /// Vertex AI with OAuth2/ADC authentication.
    #[cfg(feature = "vertex-live")]
    Vertex {
        /// Google Cloud credentials for OAuth2 token generation.
        credentials: google_cloud_auth::credentials::Credentials,
        /// Google Cloud region (e.g., "us-central1").
        region: String,
        /// Google Cloud project ID.
        project_id: String,
    },
}

impl GeminiLiveBackend {
    /// Create a Studio backend with API key authentication.
    pub fn studio(api_key: impl Into<String>) -> Self {
        Self::Studio { api_key: api_key.into() }
    }

    /// Create a Vertex AI backend using Application Default Credentials (ADC).
    ///
    /// This is the most ergonomic way to connect to Vertex AI Live. It
    /// automatically discovers credentials from the environment using
    /// `google-cloud-auth`'s default credential chain (environment variables,
    /// `gcloud auth application-default login`, service account files, etc.).
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let backend = GeminiLiveBackend::vertex_adc("my-project", "us-central1")?;
    /// let model = GeminiRealtimeModel::new(backend, "models/gemini-live-2.5-flash-native-audio");
    /// ```
    #[cfg(feature = "vertex-live")]
    pub fn vertex_adc(project_id: impl Into<String>, region: impl Into<String>) -> Result<Self> {
        let credentials =
            google_cloud_auth::credentials::Builder::default().build().map_err(|e| {
                RealtimeError::AuthError(format!(
                    "Failed to discover Application Default Credentials: {e}"
                ))
            })?;
        Ok(Self::Vertex { credentials, region: region.into(), project_id: project_id.into() })
    }
}

// ── Wire format types ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GeminiClientMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    setup: Option<GeminiSetup>,
    #[serde(skip_serializing_if = "Option::is_none")]
    realtime_input: Option<GeminiRealtimeInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_response: Option<GeminiToolResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_content: Option<GeminiClientContent>,
}

/// Configuration for Gemini 2.5 Live session resumption.
///
/// See the official documentation for details:
/// https://ai.google.dev/gemini-api/docs/live-api/session-management
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionResumptionConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiSetup {
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cached_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_resumption: Option<SessionResumptionConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GeminiContent {
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GeminiPart {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    inline_data: Option<GeminiInlineData>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiInlineData {
    mime_type: String,
    data: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiRealtimeInput {
    #[serde(skip_serializing_if = "Option::is_none")]
    media_chunks: Option<Vec<GeminiMediaChunk>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiMediaChunk {
    mime_type: String,
    data: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiToolResponse {
    function_responses: Vec<GeminiFunctionResponse>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiFunctionResponse {
    id: String,
    response: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiClientContent {
    turns: Vec<GeminiTurn>,
    turn_complete: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiTurn {
    role: String,
    parts: Vec<GeminiPart>,
}

// ── Session implementation ──────────────────────────────────────────────

/// Gemini Live session.
///
/// Manages a WebSocket connection to Google's Gemini Live API.
pub struct GeminiRealtimeSession {
    session_id: String,
    connected: Arc<AtomicBool>,
    sender: Arc<Mutex<WsSink>>,
    receiver: Arc<Mutex<WsSource>>,
    audio_buffer: Arc<Mutex<Vec<u8>>>,
}

impl GeminiRealtimeSession {
    /// Connect to Gemini Live API using the specified backend.
    pub async fn connect(
        backend: GeminiLiveBackend,
        model: &str,
        config: RealtimeConfig,
    ) -> Result<Self> {
        let ws_stream = match &backend {
            GeminiLiveBackend::Studio { api_key } => {
                let url = format!(
                    "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1alpha.GenerativeService.BidiGenerateContent?key={}",
                    api_key
                );
                let request = url.into_client_request().map_err(|e| {
                    RealtimeError::connection(format!("Failed to create request: {}", e))
                })?;
                let (ws, _) = connect_async(request).await.map_err(|e| {
                    RealtimeError::connection(format!("WebSocket connect error: {}", e))
                })?;
                ws
            }
            #[cfg(feature = "vertex-live")]
            GeminiLiveBackend::Vertex { credentials, region, project_id } => {
                let url = build_vertex_live_url(region, project_id)?;

                // Obtain OAuth2 bearer token from ADC credentials
                let header_map =
                    match credentials.headers(Default::default()).await.map_err(|e| {
                        RealtimeError::AuthError(format!(
                            "Failed to obtain OAuth2 token from ADC credentials: {e}"
                        ))
                    })? {
                        google_cloud_auth::credentials::CacheableResource::New { data, .. } => data,
                        google_cloud_auth::credentials::CacheableResource::NotModified => {
                            return Err(RealtimeError::AuthError(
                            "ADC credentials returned NotModified with no cached token available"
                                .to_string(),
                        ));
                        }
                    };

                // Extract the Authorization header value
                let auth_value = header_map
                    .get("authorization")
                    .ok_or_else(|| {
                        RealtimeError::AuthError(
                            "ADC credentials did not produce an Authorization header".to_string(),
                        )
                    })?
                    .to_str()
                    .map_err(|e| {
                        RealtimeError::AuthError(format!(
                            "Authorization header contains non-ASCII characters: {e}"
                        ))
                    })?
                    .to_string();

                // Build a WebSocket request with the Authorization header
                let mut request = url.into_client_request().map_err(|e| {
                    RealtimeError::connection(format!("Failed to create request: {e}"))
                })?;
                request.headers_mut().insert(
                    "Authorization",
                    auth_value.parse().map_err(
                        |e: tokio_tungstenite::tungstenite::http::header::InvalidHeaderValue| {
                            RealtimeError::AuthError(format!(
                                "Failed to parse Authorization header value: {e}"
                            ))
                        },
                    )?,
                );

                let (ws, _) = connect_async(request).await.map_err(|e| {
                    RealtimeError::connection(format!(
                        "Vertex AI Live WebSocket connect error: {e}"
                    ))
                })?;
                ws
            }
        };

        let (sink, source) = ws_stream.split();
        let session_id = uuid::Uuid::new_v4().to_string();

        let session = Self {
            session_id,
            connected: Arc::new(AtomicBool::new(true)),
            sender: Arc::new(Mutex::new(sink)),
            receiver: Arc::new(Mutex::new(source)),
            audio_buffer: Arc::new(Mutex::new(Vec::new())),
        };

        session.send_setup(model, config).await?;
        Ok(session)
    }

    /// Flush any buffered audio to the server.
    async fn flush_audio(&self) -> Result<()> {
        let mut buffer = self.audio_buffer.lock().await;
        if !buffer.is_empty() {
            let data = std::mem::take(&mut *buffer);
            drop(buffer);

            // Assume standard Gemini format (16kHz PCM)
            let chunk = AudioChunk::pcm16_16khz(data);
            self.send_audio_base64(&chunk.to_base64()).await?;
        }
        Ok(())
    }

    /// Send initial setup message.
    async fn send_setup(&self, model: &str, config: RealtimeConfig) -> Result<()> {
        let mut generation_config = json!({
            "responseModalities": config.modalities.unwrap_or_else(|| vec!["AUDIO".to_string()]),
        });

        if let Some(voice) = &config.voice {
            generation_config["speechConfig"] = json!({
                "voiceConfig": {
                    "prebuiltVoiceConfig": {
                        "voiceName": voice
                    }
                }
            });
        }

        if let Some(temp) = config.temperature {
            generation_config["temperature"] = json!(temp);
        }

        let system_instruction = config.instruction.map(|text| GeminiContent {
            parts: vec![GeminiPart { text: Some(text), inline_data: None }],
        });

        let tools = convert_tools(config.tools);

        // Functionally extract the token if it exists in the prior state map
        let handle = config
            .extra
            .as_ref()
            .and_then(|ext| ext.get("resumeToken"))
            .and_then(|val| val.as_str())
            .map(|s| s.to_string());

        // Always attach the config object to explicitly enable the session resumption feature,
        // even if the handle is currently None.
        let session_resumption = Some(SessionResumptionConfig { handle });

        let setup = GeminiClientMessage {
            setup: Some(GeminiSetup {
                model: model.to_string(),
                system_instruction,
                generation_config: Some(generation_config),
                tools,
                cached_content: config.cached_content,
                session_resumption,
            }),
            realtime_input: None,
            tool_response: None,
            client_content: None,
        };

        tracing::info!(model_id = %model, "Sending Gemini Live setup");
        self.send_raw(&setup).await
    }

    /// Send a raw message.
    async fn send_raw<T: Serialize>(&self, value: &T) -> Result<()> {
        let msg = serde_json::to_string(value)
            .map_err(|e| RealtimeError::protocol(format!("JSON serialize error: {}", e)))?;

        let mut sender = self.sender.lock().await;
        sender
            .send(Message::Text(msg.into()))
            .await
            .map_err(|e| RealtimeError::connection(format!("Send error: {}", e)))?;

        Ok(())
    }

    /// Receive and parse the next message.
    async fn receive_raw(&self) -> Option<Result<ServerEvent>> {
        let mut receiver = self.receiver.lock().await;

        match receiver.next().await {
            Some(Ok(Message::Text(text))) => match self.translate_gemini_event(&text) {
                Ok(event) => Some(Ok(event)),
                Err(e) => Some(Err(e)),
            },
            Some(Ok(Message::Binary(bytes))) => match String::from_utf8(bytes.to_vec()) {
                Ok(text) => match self.translate_gemini_event(&text) {
                    Ok(event) => Some(Ok(event)),
                    Err(e) => Some(Err(e)),
                },
                Err(e) => Some(Err(RealtimeError::protocol(format!(
                    "Invalid UTF-8 in binary message: {}",
                    e
                )))),
            },
            Some(Ok(Message::Close(_))) => {
                self.connected.store(false, Ordering::SeqCst);
                None
            }
            Some(Ok(_)) => Some(Ok(ServerEvent::Unknown)),
            Some(Err(e)) => {
                self.connected.store(false, Ordering::SeqCst);
                Some(Err(RealtimeError::connection(format!("Receive error: {}", e))))
            }
            None => {
                self.connected.store(false, Ordering::SeqCst);
                None
            }
        }
    }

    /// Translate Gemini-specific events to unified format.
    fn translate_gemini_event(&self, raw: &str) -> Result<ServerEvent> {
        tracing::debug!(%raw, "Translating Gemini event");
        let value: Value = serde_json::from_str(raw)
            .map_err(|e| RealtimeError::protocol(format!("Parse error: {}, raw: {}", e, raw)))?;

        // Check for setup completion
        if let Some(_setup_complete) = value.get("setupComplete") {
            return Ok(ServerEvent::SessionCreated {
                event_id: uuid::Uuid::new_v4().to_string(),
                session: value.clone(),
            });
        }

        // Check for server content (audio/text)
        if let Some(content) = value.get("serverContent") {
            if let Some(turn_complete) = content.get("turnComplete") {
                if turn_complete.as_bool().unwrap_or(false) {
                    return Ok(ServerEvent::ResponseDone {
                        event_id: uuid::Uuid::new_v4().to_string(),
                        response: value.clone(),
                    });
                }
            }

            if let Some(parts) = content.get("modelTurn").and_then(|t| t.get("parts")) {
                if let Some(parts_arr) = parts.as_array() {
                    for part in parts_arr {
                        // Audio output — decode base64 to raw bytes
                        if let Some(inline_data) = part.get("inlineData") {
                            if let Some(data) = inline_data.get("data").and_then(|d| d.as_str()) {
                                let decoded = base64::engine::general_purpose::STANDARD
                                    .decode(data)
                                    .unwrap_or_default();
                                return Ok(ServerEvent::AudioDelta {
                                    event_id: uuid::Uuid::new_v4().to_string(),
                                    response_id: String::new(),
                                    item_id: String::new(),
                                    output_index: 0,
                                    content_index: 0,
                                    delta: decoded,
                                });
                            }
                        }
                        // Text output
                        if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                            return Ok(ServerEvent::TextDelta {
                                event_id: uuid::Uuid::new_v4().to_string(),
                                response_id: String::new(),
                                item_id: String::new(),
                                output_index: 0,
                                content_index: 0,
                                delta: text.to_string(),
                            });
                        }
                    }
                }
            }
        }

        // Catch the Server Update for sessionResumptionUpdate
        // Note the intentional protocol asymmetry here: the client sends the parameter as handle,
        // but the server transmits the parameter back as resumptionToken.
        // Reference: https://ai.google.dev/gemini-api/docs/live-api/session-management
        if let Some(resumption_update) = value.get("sessionResumptionUpdate") {
            if let Some(token) = resumption_update.get("resumptionToken").and_then(|t| t.as_str()) {
                tracing::debug!("Received new Gemini 2.5 Native resumption token");
                return Ok(ServerEvent::SessionUpdated {
                    event_id: uuid::Uuid::new_v4().to_string(),
                    session: json!({ "resumeToken": token }),
                });
            }
        }

        // Check for tool calls
        if let Some(tool_call) = value.get("toolCall") {
            if let Some(calls) = tool_call.get("functionCalls").and_then(|c| c.as_array()) {
                if let Some(call) = calls.first() {
                    let name = call.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let id = call.get("id").and_then(|i| i.as_str()).unwrap_or("");
                    let args = call.get("args").cloned().unwrap_or(json!({}));

                    return Ok(ServerEvent::FunctionCallDone {
                        event_id: uuid::Uuid::new_v4().to_string(),
                        response_id: String::new(),
                        item_id: String::new(),
                        output_index: 0,
                        call_id: id.to_string(),
                        name: name.to_string(),
                        arguments: serde_json::to_string(&args).unwrap_or_default(),
                    });
                }
            }
        }

        Ok(ServerEvent::Unknown)
    }
}

#[async_trait]
impl RealtimeSession for GeminiRealtimeSession {
    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    async fn send_audio(&self, audio: &AudioChunk) -> Result<()> {
        // Smart Audio Buffering: buffer small chunks to avoid overhead
        let mut buffer = self.audio_buffer.lock().await;
        buffer.extend_from_slice(&audio.data);

        // 3200 bytes = 100ms at 16kHz 16-bit mono
        if buffer.len() >= 3200 {
            let data = std::mem::take(&mut *buffer);
            drop(buffer); // Release lock before sending

            // We use the format from the current chunk, assuming consistency
            let chunk = AudioChunk::new(data, audio.format.clone());
            self.send_audio_base64(&chunk.to_base64()).await?;
        }
        Ok(())
    }

    async fn send_audio_base64(&self, audio_base64: &str) -> Result<()> {
        let msg = GeminiClientMessage {
            setup: None,
            realtime_input: Some(GeminiRealtimeInput {
                media_chunks: Some(vec![GeminiMediaChunk {
                    mime_type: "audio/pcm".to_string(),
                    data: audio_base64.to_string(),
                }]),
                text: None,
            }),
            tool_response: None,
            client_content: None,
        };
        self.send_raw(&msg).await
    }

    async fn send_text(&self, text: &str) -> Result<()> {
        // Use client_content with turns (correct Gemini Live API format)
        let msg = GeminiClientMessage {
            setup: None,
            realtime_input: None,
            tool_response: None,
            client_content: Some(GeminiClientContent {
                turns: vec![GeminiTurn {
                    role: "user".to_string(),
                    parts: vec![GeminiPart { text: Some(text.to_string()), inline_data: None }],
                }],
                turn_complete: true,
            }),
        };
        self.send_raw(&msg).await
    }

    async fn send_tool_response(&self, response: ToolResponse) -> Result<()> {
        let output = match &response.output {
            Value::String(s) => json!({ "result": s }),
            other => other.clone(),
        };

        let msg = GeminiClientMessage {
            setup: None,
            realtime_input: None,
            tool_response: Some(GeminiToolResponse {
                function_responses: vec![GeminiFunctionResponse {
                    id: response.call_id,
                    response: output,
                }],
            }),
            client_content: None,
        };
        self.send_raw(&msg).await
    }

    async fn commit_audio(&self) -> Result<()> {
        self.flush_audio().await
    }

    async fn clear_audio(&self) -> Result<()> {
        let mut buffer = self.audio_buffer.lock().await;
        buffer.clear();
        Ok(())
    }

    async fn create_response(&self) -> Result<()> {
        Ok(()) // Gemini auto-generates responses
    }

    async fn interrupt(&self) -> Result<()> {
        // Strategic flush: clear any buffered audio that hasn't been sent
        self.clear_audio().await?;
        Ok(()) // Gemini handles interruption via VAD
    }

    async fn send_event(&self, event: ClientEvent) -> Result<()> {
        match event {
            // Intercept standard messages from the orchestrator
            ClientEvent::Message { role, parts } => {
                let msg = translate_client_message(&role, parts);
                tracing::info!(role = ?role, "Injecting mid-flight context via native adk-rust types");
                self.send_raw(&msg).await
            }

            // Explicitly handle all other unified ClientEvent variants.
            // Returning Ok(()) silently for unsupported features is an anti-pattern.
            // We log a clear warning that Gemini does not natively support this specific control event.
            ClientEvent::AudioDelta { .. } => {
                tracing::warn!(
                    "AudioDelta is explicitly handled via `send_audio`, not `send_event`. Dropping event."
                );
                Ok(())
            }
            ClientEvent::InputAudioBufferCommit => {
                tracing::warn!(
                    "Gemini Live API does not support manual audio buffer commits. Dropping event."
                );
                Ok(())
            }
            ClientEvent::InputAudioBufferClear => {
                tracing::warn!(
                    "Gemini Live API does not support manual audio buffer clears via wire events. Dropping event."
                );
                Ok(())
            }
            ClientEvent::ConversationItemCreate { .. } => {
                tracing::warn!(
                    "Raw ConversationItemCreate is an OpenAI construct. Use ClientEvent::Message instead for Gemini. Dropping event."
                );
                Ok(())
            }
            ClientEvent::ResponseCreate { .. } => {
                tracing::warn!(
                    "Gemini Live API automatically generates responses based on VAD/turns. Manual ResponseCreate is unsupported. Dropping event."
                );
                Ok(())
            }
            ClientEvent::ResponseCancel => {
                tracing::warn!(
                    "Gemini Live API natively handles interruption via VAD. Manual ResponseCancel is unsupported. Dropping event."
                );
                Ok(())
            }
            ClientEvent::SessionUpdate { .. } => {
                tracing::warn!(
                    "Raw SessionUpdate is an OpenAI construct. Use RealtimeRunner's `update_session` for provider-agnostic Context Mutation. Dropping event."
                );
                Ok(())
            }
            ClientEvent::UpdateSession { .. } => {
                tracing::error!(
                    "Internal UpdateSession intent leaked to the Gemini transport socket. This should have been intercepted by the RealtimeRunner."
                );
                Err(RealtimeError::ProviderError("Internal intent leaked to transport".to_string()))
            }
        }
    }

    async fn next_event(&self) -> Option<Result<ServerEvent>> {
        self.receive_raw().await
    }

    fn events(&self) -> Pin<Box<dyn Stream<Item = Result<ServerEvent>> + Send + '_>> {
        Box::pin(async_stream::stream! {
            while self.is_connected() {
                match self.receive_raw().await {
                    Some(Ok(event)) => yield Ok(event),
                    Some(Err(e)) => yield Err(e),
                    None => break,
                }
            }
        })
    }

    async fn close(&self) -> Result<()> {
        self.connected.store(false, Ordering::SeqCst);
        let mut sender = self.sender.lock().await;
        let _ = sender.send(Message::Close(None)).await;
        Ok(())
    }

    async fn mutate_context(
        &self,
        config: crate::config::RealtimeConfig,
    ) -> Result<ContextMutationOutcome> {
        tracing::info!(
            "Gemini API does not support native mid-flight context swaps; signalling resumption needed."
        );
        Ok(ContextMutationOutcome::RequiresResumption(config))
    }
}

impl std::fmt::Debug for GeminiRealtimeSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeminiRealtimeSession")
            .field("session_id", &self.session_id)
            .field("connected", &self.connected.load(Ordering::SeqCst))
            .finish()
    }
}

/// Construct the Vertex AI Live WebSocket URL from region and project ID.
///
/// Returns `RealtimeError::ConfigError` if region or project_id is empty.
#[cfg(feature = "vertex-live")]
pub fn build_vertex_live_url(region: &str, project_id: &str) -> Result<String> {
    if region.is_empty() {
        return Err(RealtimeError::config("Vertex AI Live requires a non-empty region"));
    }
    if project_id.is_empty() {
        return Err(RealtimeError::config("Vertex AI Live requires a non-empty project_id"));
    }
    Ok(format!(
        "wss://{region}-aiplatform.googleapis.com/ws/\
         google.cloud.aiplatform.v1beta1.LlmBidiService/BidiGenerateContent\
         ?project_id={project_id}",
    ))
}

/// Pure translation function for converting a standard `adk_core` message into
/// Gemini Live API's native `clientContent` payload.
pub(crate) fn translate_client_message(
    role: &str,
    parts: Vec<adk_core::types::Part>,
) -> GeminiClientMessage {
    // 1. Translate the polymorphic `adk_core::types::Part` elements into strictly-typed `GeminiPart` structures.
    let mut gemini_parts: Vec<GeminiPart> = Vec::new();
    for p in parts {
        match p {
            adk_core::types::Part::Text { text } => {
                gemini_parts.push(GeminiPart { text: Some(text), inline_data: None });
            }
            adk_core::types::Part::InlineData { mime_type, data } => {
                // Gemini natively encodes binary artifacts (images/audio) via a base64 payload envelope.
                let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
                gemini_parts.push(GeminiPart {
                    text: None,
                    inline_data: Some(GeminiInlineData { mime_type, data: encoded }),
                });
            }

            // 2. Gracefully skip semantic elements that Google's Live API `clientContent` turn does not support
            // using `tracing::warn!`, avoiding "silent data loss" or injecting empty string placeholders.
            adk_core::types::Part::FileData { file_uri, .. } => {
                tracing::warn!(
                    "Dropping unsupported FileData part in Gemini session: {}",
                    file_uri
                );
            }
            adk_core::types::Part::Thinking { .. } => {
                tracing::warn!("Dropping unsupported Thinking part in Gemini session");
            }
            adk_core::types::Part::FunctionCall { name, .. } => {
                tracing::warn!(
                    "Dropping unsupported FunctionCall part in Gemini session: {}",
                    name
                );
            }
            adk_core::types::Part::FunctionResponse { .. } => {
                tracing::warn!("Dropping unsupported FunctionResponse part in Gemini session");
            }
            adk_core::types::Part::ServerToolCall { .. } => {
                tracing::warn!("Dropping unsupported ServerToolCall part in Gemini session");
            }
            adk_core::types::Part::ServerToolResponse { .. } => {
                tracing::warn!("Dropping unsupported ServerToolResponse part in Gemini session");
            }
        }
    }

    // 3. Coerce the `Role`.
    // Gemini Live's bidirectional socket strongly rejects `system` or `developer` roles
    // inside mid-flight `clientContent` turns. To support Cognitive Handoffs, we intercept
    // the system instruction and safely masquerade it as a high-priority "user" turn.
    let (gemini_role, final_parts) = match role {
        "system" | "developer" => {
            let mut modified_parts = gemini_parts;
            let mut text_injected = false;

            // Iterate to find the first actual text element in the user's prompt (avoiding images/audio arrays)
            // to safely inject the system prefix.
            for part in modified_parts.iter_mut() {
                if let Some(ref mut text) = part.text {
                    *text = format!("[CRITICAL SYSTEM DIRECTIVE OVERRIDE]\n{}", text);
                    text_injected = true;
                    break;
                }
            }

            // If the orchestrator sent a `system` role containing exclusively non-text data (e.g., just an image),
            // construct a synthetic text part to carry the directive.
            if !text_injected {
                modified_parts.insert(
                    0,
                    GeminiPart {
                        text: Some("[CRITICAL SYSTEM DIRECTIVE OVERRIDE]".to_string()),
                        inline_data: None,
                    },
                );
            }

            ("user".to_string(), modified_parts)
        }
        "user" => ("user".to_string(), gemini_parts),
        "model" | "assistant" => ("model".to_string(), gemini_parts),
        _ => ("user".to_string(), gemini_parts), // Default fallback for custom orchestrator roles
    };

    // 4. Construct the native `GeminiClientContent` wire envelope.
    GeminiClientMessage {
        setup: None,
        realtime_input: None,
        tool_response: None,
        client_content: Some(GeminiClientContent {
            turns: vec![GeminiTurn { role: gemini_role, parts: final_parts }],
            turn_complete: true,
        }),
    }
}

/// Convert ADK tool definitions to Gemini format.
fn convert_tools(tools: Option<Vec<ToolDefinition>>) -> Option<Vec<Value>> {
    tools.map(|tools| {
        vec![json!({
            "functionDeclarations": tools.iter().map(|t| {
                let mut decl = json!({ "name": t.name });
                if let Some(desc) = &t.description {
                    decl["description"] = json!(desc);
                }
                if let Some(params) = &t.parameters {
                    decl["parameters"] = params.clone();
                }
                decl
            }).collect::<Vec<_>>()
        })]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::types::Part;

    #[test]
    fn test_gemini_translate_text_only() {
        let parts = vec![Part::Text { text: "Hello".to_string() }];
        let msg = translate_client_message("user", parts);

        let content = msg.client_content.unwrap();
        assert_eq!(content.turns.len(), 1);
        assert_eq!(content.turns[0].role, "user");

        let gemini_parts = &content.turns[0].parts;
        assert_eq!(gemini_parts.len(), 1);
        assert_eq!(gemini_parts[0].text.as_deref(), Some("Hello"));
        assert!(gemini_parts[0].inline_data.is_none());
    }

    #[test]
    fn test_gemini_translate_text_and_inline_data() {
        let parts = vec![
            Part::Text { text: "Look:".to_string() },
            Part::InlineData { mime_type: "image/png".to_string(), data: vec![0x1, 0x2] },
        ];
        let msg = translate_client_message("user", parts);

        let content = msg.client_content.unwrap();
        let gemini_parts = &content.turns[0].parts;
        assert_eq!(gemini_parts.len(), 2);

        assert_eq!(gemini_parts[0].text.as_deref(), Some("Look:"));

        let inline = gemini_parts[1].inline_data.as_ref().unwrap();
        assert_eq!(inline.mime_type, "image/png");
        assert_eq!(inline.data, "AQI="); // base64 encoded [1,2]
    }

    #[test]
    fn test_gemini_system_override_text_first() {
        let parts = vec![Part::Text { text: "Be helpful".to_string() }];
        let msg = translate_client_message("system", parts);

        let content = msg.client_content.unwrap();
        assert_eq!(content.turns[0].role, "user"); // coerced

        let gemini_parts = &content.turns[0].parts;
        assert_eq!(gemini_parts.len(), 1);
        assert_eq!(
            gemini_parts[0].text.as_deref(),
            Some("[CRITICAL SYSTEM DIRECTIVE OVERRIDE]\nBe helpful")
        );
    }

    #[test]
    fn test_gemini_system_override_non_text_first() {
        let parts = vec![
            Part::InlineData { mime_type: "image/png".to_string(), data: vec![0x1] },
            Part::Text { text: "Analyze this".to_string() },
        ];
        let msg = translate_client_message("system", parts);

        let content = msg.client_content.unwrap();
        let gemini_parts = &content.turns[0].parts;
        assert_eq!(gemini_parts.len(), 2);

        // Ensure the directive was applied to the FIRST text part, despite being index 1
        assert!(gemini_parts[0].inline_data.is_some());
        assert_eq!(
            gemini_parts[1].text.as_deref(),
            Some("[CRITICAL SYSTEM DIRECTIVE OVERRIDE]\nAnalyze this")
        );
    }

    #[test]
    fn test_gemini_system_override_no_text() {
        let parts = vec![Part::InlineData { mime_type: "image/png".to_string(), data: vec![0x1] }];
        let msg = translate_client_message("system", parts);

        let content = msg.client_content.unwrap();
        let gemini_parts = &content.turns[0].parts;
        assert_eq!(gemini_parts.len(), 2);

        // Ensure a new text part was inserted at the beginning
        assert_eq!(gemini_parts[0].text.as_deref(), Some("[CRITICAL SYSTEM DIRECTIVE OVERRIDE]"));
        assert!(gemini_parts[1].inline_data.is_some());
    }

    #[test]
    fn test_gemini_skips_unsupported_parts() {
        let parts = vec![
            Part::Text { text: "First".to_string() },
            Part::FileData {
                mime_type: "image/jpeg".to_string(),
                file_uri: "http://example.com/img".to_string(),
            }, // Should be skipped
            Part::Thinking { thinking: "Hmm".to_string(), signature: None }, // Should be skipped
            Part::Text { text: "Last".to_string() },
        ];
        let msg = translate_client_message("user", parts);

        let content = msg.client_content.unwrap();
        let gemini_parts = &content.turns[0].parts;
        assert_eq!(gemini_parts.len(), 2);

        assert_eq!(gemini_parts[0].text.as_deref(), Some("First"));
        assert_eq!(gemini_parts[1].text.as_deref(), Some("Last"));
    }
}
