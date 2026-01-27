//! Gemini Live session implementation.

use crate::audio::AudioChunk;
use crate::config::RealtimeConfig;
use crate::error::{RealtimeError, Result};
use crate::events::{ClientEvent, ServerEvent, ToolResponse};
use crate::session::RealtimeSession;
use async_trait::async_trait;
use futures::stream::Stream;
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;
use tokio_tungstenite::{connect_async, tungstenite::Message};

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsSink = futures::stream::SplitSink<WsStream, Message>;
type WsSource = futures::stream::SplitStream<WsStream>;

/// Gemini-specific client message format.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiClientMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    setup: Option<GeminiSetup>,
    #[serde(skip_serializing_if = "Option::is_none")]
    realtime_input: Option<GeminiRealtimeInput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_response: Option<GeminiToolResponse>,
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
    /// Connect to Gemini Live API.
    pub async fn connect(url: &str, model: &str, config: RealtimeConfig) -> Result<Self> {
        // Connect WebSocket
        let (ws_stream, _response) = connect_async(url)
            .await
            .map_err(|e| RealtimeError::connection(format!("WebSocket connect error: {}", e)))?;

        let (sink, source) = ws_stream.split();

        // Generate session ID
        let session_id = uuid::Uuid::new_v4().to_string();

        let session = Self {
            session_id,
            connected: Arc::new(AtomicBool::new(true)),
            sender: Arc::new(Mutex::new(sink)),
            receiver: Arc::new(Mutex::new(source)),
        };

        // Send setup message
        session.send_setup(model, config).await?;

        Ok(session)
    }

    /// Send initial setup message.
    async fn send_setup(&self, model: &str, config: RealtimeConfig) -> Result<()> {
        let system_instruction = config.instruction.map(|text| GeminiContent {
            parts: vec![GeminiPart { text: Some(text), inline_data: None }],
        });

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

        let tools = config.tools.map(|tools| {
            vec![json!({
                "functionDeclarations": tools.iter().map(|t| {
                    let mut decl = json!({
                        "name": t.name,
                    });
                    if let Some(desc) = &t.description {
                        decl["description"] = json!(desc);
                    }
                    if let Some(params) = &t.parameters {
                        decl["parameters"] = params.clone();
                    }
                    decl
                }).collect::<Vec<_>>()
            })]
        });

        let setup = GeminiClientMessage {
            setup: Some(GeminiSetup {
                model: model.to_string(),
                system_instruction,
                generation_config: Some(generation_config),
                tools,
            }),
            realtime_input: None,
            tool_response: None,
        };

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
            Some(Ok(Message::Text(text))) => {
                // Gemini has a different response format, translate to unified events
                match self.translate_gemini_event(&text) {
                    Ok(event) => Some(Ok(event)),
                    Err(e) => Some(Err(e)),
                }
            }
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
        let value: Value = serde_json::from_str(raw)
            .map_err(|e| RealtimeError::protocol(format!("Parse error: {}", e)))?;

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
                        // Audio output
                        if let Some(inline_data) = part.get("inlineData") {
                            if let Some(data) = inline_data.get("data").and_then(|d| d.as_str()) {
                                return Ok(ServerEvent::AudioDelta {
                                    event_id: uuid::Uuid::new_v4().to_string(),
                                    response_id: String::new(),
                                    item_id: String::new(),
                                    output_index: 0,
                                    content_index: 0,
                                    delta: data.to_string(),
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
        };
        self.send_raw(&msg).await
    }

    async fn send_text(&self, text: &str) -> Result<()> {
        let msg = GeminiClientMessage {
            setup: None,
            realtime_input: Some(GeminiRealtimeInput {
                media_chunks: None,
                text: Some(text.to_string()),
            }),
            tool_response: None,
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
        };
        self.send_raw(&msg).await
    }

    async fn commit_audio(&self) -> Result<()> {
        // Gemini handles this automatically with server VAD
        Ok(())
    }

    async fn clear_audio(&self) -> Result<()> {
        // Not directly supported, but session can be interrupted
        Ok(())
    }

    async fn create_response(&self) -> Result<()> {
        // Gemini generates responses automatically
        Ok(())
    }

    async fn interrupt(&self) -> Result<()> {
        // Send an interruption signal (implementation depends on Gemini API)
        Ok(())
    }

    async fn send_event(&self, _event: ClientEvent) -> Result<()> {
        // Gemini uses a different event format
        Err(RealtimeError::provider("Raw ClientEvent not supported for Gemini"))
    }

    async fn next_event(&self) -> Option<Result<ServerEvent>> {
        self.receive_raw().await
    }

    fn events(&self) -> Pin<Box<dyn Stream<Item = Result<ServerEvent>> + Send + '_>> {
        Box::pin(futures::stream::unfold(self, |session| async move {
            let event = session.receive_raw().await?;
            Some((event, session))
        }))
    }

    async fn close(&self) -> Result<()> {
        self.connected.store(false, Ordering::SeqCst);

        let mut sender = self.sender.lock().await;
        sender
            .send(Message::Close(None))
            .await
            .map_err(|e| RealtimeError::connection(format!("Close error: {}", e)))?;

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
