//! Gemini Live session implementation.
//!
//! Manages a WebSocket connection to Google's Gemini Live API with support
//! for both AI Studio (API key) and Vertex AI (OAuth/ADC) backends.

use crate::audio::AudioChunk;
use crate::config::{RealtimeConfig, ToolDefinition};
use crate::error::{RealtimeError, Result};
use crate::events::{ClientEvent, ServerEvent, ToolResponse};
use crate::session::RealtimeSession;
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
}

// ── Wire format types ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiClientMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    setup: Option<GeminiSetup>,
    #[serde(skip_serializing_if = "Option::is_none")]
    realtime_input: Option<GeminiRealtimeInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_response: Option<GeminiToolResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_content: Option<GeminiClientContent>,
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
        };

        let (sink, source) = ws_stream.split();
        let session_id = uuid::Uuid::new_v4().to_string();

        let session = Self {
            session_id,
            connected: Arc::new(AtomicBool::new(true)),
            sender: Arc::new(Mutex::new(sink)),
            receiver: Arc::new(Mutex::new(source)),
        };

        session.send_setup(model, config).await?;
        Ok(session)
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

        let setup = GeminiClientMessage {
            setup: Some(GeminiSetup {
                model: model.to_string(),
                system_instruction,
                generation_config: Some(generation_config),
                tools,
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
        if value.get("setupComplete").is_some() {
            return Ok(ServerEvent::SessionCreated {
                event_id: uuid::Uuid::new_v4().to_string(),
                session: value,
            });
        }

        // Check for server content (audio/text)
        if let Some(content) = value.get("serverContent") {
            if let Some(turn_complete) = content.get("turnComplete") {
                if turn_complete.as_bool().unwrap_or(false) {
                    return Ok(ServerEvent::ResponseDone {
                        event_id: uuid::Uuid::new_v4().to_string(),
                        response: value,
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
        self.send_audio_base64(&audio.to_base64()).await
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
        Ok(()) // Gemini auto-commits
    }

    async fn clear_audio(&self) -> Result<()> {
        Ok(()) // Not supported by Gemini Live
    }

    async fn create_response(&self) -> Result<()> {
        Ok(()) // Gemini auto-generates responses
    }

    async fn interrupt(&self) -> Result<()> {
        Ok(()) // Gemini handles interruption via VAD
    }

    async fn send_event(&self, _event: ClientEvent) -> Result<()> {
        Ok(()) // Raw events not directly supported
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
}

impl std::fmt::Debug for GeminiRealtimeSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeminiRealtimeSession")
            .field("session_id", &self.session_id)
            .field("connected", &self.connected.load(Ordering::SeqCst))
            .finish()
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
