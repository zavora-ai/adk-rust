//! OpenAI Realtime session implementation.

use crate::audio::AudioChunk;
use crate::config::RealtimeConfig;
use crate::error::{RealtimeError, Result};
use crate::events::{ClientEvent, ServerEvent, ToolResponse};
use crate::session::{ContextMutationOutcome, RealtimeSession};
use async_trait::async_trait;
use futures::stream::Stream;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        Message,
        http::{Request, Uri},
    },
};

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsSink = futures::stream::SplitSink<WsStream, Message>;
type WsSource = futures::stream::SplitStream<WsStream>;

/// OpenAI Realtime session.
///
/// Manages a WebSocket connection to OpenAI's Realtime API.
pub struct OpenAIRealtimeSession {
    session_id: String,
    connected: Arc<AtomicBool>,
    sender: Arc<Mutex<WsSink>>,
    receiver: Arc<Mutex<WsSource>>,
}

impl OpenAIRealtimeSession {
    /// Connect to OpenAI Realtime API.
    pub async fn connect(url: &str, api_key: &str, config: RealtimeConfig) -> Result<Self> {
        // Parse URL and build request with auth header
        let uri: Uri =
            url.parse().map_err(|e| RealtimeError::connection(format!("Invalid URL: {}", e)))?;

        let host = uri.host().unwrap_or("api.openai.com");

        let request = Request::builder()
            .uri(url)
            .header("Host", host)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("OpenAI-Beta", "realtime=v1")
            .header("Sec-WebSocket-Key", generate_ws_key())
            .header("Sec-WebSocket-Version", "13")
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .body(())
            .map_err(|e| RealtimeError::connection(format!("Request build error: {}", e)))?;

        // Connect WebSocket
        let (ws_stream, _response) = connect_async(request)
            .await
            .map_err(|e| RealtimeError::connection(format!("WebSocket connect error: {}", e)))?;

        let (sink, source) = ws_stream.split();

        // Generate session ID (will be updated when we receive session.created)
        let session_id = uuid::Uuid::new_v4().to_string();

        let session = Self {
            session_id,
            connected: Arc::new(AtomicBool::new(true)),
            sender: Arc::new(Mutex::new(sink)),
            receiver: Arc::new(Mutex::new(source)),
        };

        // Send initial session configuration
        session.configure_session(config).await?;

        Ok(session)
    }

    /// Configure the session with initial settings.
    async fn configure_session(&self, config: RealtimeConfig) -> Result<()> {
        let mut session_config = json!({});

        if let Some(instruction) = &config.instruction {
            session_config["instructions"] = json!(instruction);
        }

        if let Some(voice) = &config.voice {
            session_config["voice"] = json!(voice);
        }

        if let Some(modalities) = &config.modalities {
            session_config["modalities"] = json!(modalities);
        }

        if let Some(input_format) = &config.input_audio_format {
            session_config["input_audio_format"] = json!(input_format.to_string());
        }

        if let Some(output_format) = &config.output_audio_format {
            session_config["output_audio_format"] = json!(output_format.to_string());
        }

        if let Some(vad) = &config.turn_detection {
            let vad_config = match vad.mode {
                crate::config::VadMode::ServerVad => {
                    let mut cfg = json!({
                        "type": "server_vad"
                    });
                    if let Some(ms) = vad.silence_duration_ms {
                        cfg["silence_duration_ms"] = json!(ms);
                    }
                    if let Some(thresh) = vad.threshold {
                        cfg["threshold"] = json!(thresh);
                    }
                    if let Some(prefix) = vad.prefix_padding_ms {
                        cfg["prefix_padding_ms"] = json!(prefix);
                    }
                    cfg
                }
                crate::config::VadMode::SemanticVad => {
                    let mut cfg = json!({
                        "type": "semantic_vad"
                    });
                    if let Some(eagerness) = &vad.eagerness {
                        cfg["eagerness"] = json!(eagerness);
                    }
                    cfg
                }
                crate::config::VadMode::None => {
                    json!(null)
                }
            };
            session_config["turn_detection"] = vad_config;
        }

        if let Some(tools) = &config.tools {
            let tool_defs: Vec<Value> = tools
                .iter()
                .map(|t| {
                    let mut def = json!({
                        "type": "function",
                        "name": t.name,
                    });
                    if let Some(desc) = &t.description {
                        def["description"] = json!(desc);
                    }
                    if let Some(params) = &t.parameters {
                        def["parameters"] = params.clone();
                    }
                    def
                })
                .collect();
            session_config["tools"] = json!(tool_defs);
        }

        if let Some(temp) = config.temperature {
            session_config["temperature"] = json!(temp);
        }

        if let Some(max_tokens) = config.max_response_output_tokens {
            session_config["max_response_output_tokens"] = json!(max_tokens);
        }

        if let Some(transcription) = &config.input_audio_transcription {
            session_config["input_audio_transcription"] = json!({
                "model": transcription.model
            });
        }

        // Send session.update event
        let event = json!({
            "type": "session.update",
            "session": session_config
        });

        self.send_raw(&event).await
    }

    /// Send a raw JSON message.
    async fn send_raw(&self, value: &Value) -> Result<()> {
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
            Some(Ok(Message::Text(text))) => match serde_json::from_str::<ServerEvent>(&text) {
                Ok(event) => Some(Ok(event)),
                Err(e) => Some(Err(RealtimeError::protocol(format!(
                    "Parse error: {} - {}",
                    e,
                    &text[..text.len().min(200)]
                )))),
            },
            Some(Ok(Message::Close(_))) => {
                self.connected.store(false, Ordering::SeqCst);
                None
            }
            Some(Ok(_)) => {
                // Ignore ping/pong/binary
                Some(Ok(ServerEvent::Unknown))
            }
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
}

#[async_trait]
impl RealtimeSession for OpenAIRealtimeSession {
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
        let event = json!({
            "type": "input_audio_buffer.append",
            "audio": audio_base64
        });
        self.send_raw(&event).await
    }

    async fn send_text(&self, text: &str) -> Result<()> {
        let event = json!({
            "type": "conversation.item.create",
            "item": {
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": text
                }]
            }
        });
        self.send_raw(&event).await
    }

    async fn send_tool_response(&self, response: ToolResponse) -> Result<()> {
        let output = match &response.output {
            Value::String(s) => s.clone(),
            other => serde_json::to_string(other).unwrap_or_default(),
        };

        let event = json!({
            "type": "conversation.item.create",
            "item": {
                "type": "function_call_output",
                "call_id": response.call_id,
                "output": output
            }
        });
        self.send_raw(&event).await?;

        // Trigger response after tool output
        self.create_response().await
    }

    async fn commit_audio(&self) -> Result<()> {
        let event = json!({
            "type": "input_audio_buffer.commit"
        });
        self.send_raw(&event).await
    }

    async fn clear_audio(&self) -> Result<()> {
        let event = json!({
            "type": "input_audio_buffer.clear"
        });
        self.send_raw(&event).await
    }

    async fn create_response(&self) -> Result<()> {
        let event = json!({
            "type": "response.create"
        });
        self.send_raw(&event).await
    }

    async fn interrupt(&self) -> Result<()> {
        let event = json!({
            "type": "response.cancel"
        });
        self.send_raw(&event).await
    }

    async fn send_event(&self, event: ClientEvent) -> Result<()> {
        match event {
            ClientEvent::Message { role, parts } => {
                let payload = translate_client_message(&role, parts);
                tracing::info!(role = ?role, "Injecting mid-flight context via native adk-rust types");
                self.send_raw(&payload).await
            }
            ClientEvent::UpdateSession { .. } => {
                tracing::error!(
                    "Internal UpdateSession intent leaked to the OpenAI transport socket. This should have been intercepted by the RealtimeRunner."
                );
                Err(RealtimeError::ProviderError("Internal intent leaked to transport".to_string()))
            }
            other => {
                // OpenAI Realtime API is heavily integrated with the generic `ClientEvent` payload structure.
                // We serialize the remaining variants dynamically.
                let value = serde_json::to_value(&other)
                    .map_err(|e| RealtimeError::protocol(format!("Serialize error: {}", e)))?;
                self.send_raw(&value).await
            }
        }
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

    async fn mutate_context(
        &self,
        config: crate::config::RealtimeConfig,
    ) -> Result<ContextMutationOutcome> {
        tracing::info!("Updating OpenAI Realtime session context natively");
        self.configure_session(config).await?;
        Ok(ContextMutationOutcome::Applied)
    }
}

/// Pure translation function for converting a standard `adk_core` message into
/// OpenAI Realtime API's native `conversation.item.create` payload.
pub(crate) fn translate_client_message(role: &str, parts: Vec<adk_core::types::Part>) -> Value {
    // 1. Coerce the internal `adk_core` roles into the strict literal strings
    // supported by OpenAI Realtime's `conversation.item.create` schema.
    let openai_role = match role {
        "system" | "developer" => "system",
        "user" => "user",
        "model" | "assistant" => "assistant",
        _ => "user", // Default fallback for custom roles
    };

    // 2. Map the polymorphic `adk_core::types::Part` elements into JSON objects.
    let mut content: Vec<Value> = Vec::new();
    for p in parts {
        match p {
            adk_core::types::Part::Text { text } => {
                content.push(json!({
                    "type": "input_text",
                    "text": text
                }));
            }
            adk_core::types::Part::InlineData { mime_type, data } => {
                // OpenAI Realtime API accepts "input_audio" natively for base64 audio.
                // It does not support native inline image data via this specific websocket frame.
                if mime_type.starts_with("audio/") {
                    use base64::Engine;
                    let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
                    content.push(json!({
                        "type": "input_audio",
                        "audio": encoded
                    }));
                } else {
                    tracing::warn!(
                        "Dropping unsupported InlineData (non-audio) part in OpenAI session: {}",
                        mime_type
                    );
                }
            }

            // 3. Gracefully skip unsupported semantic features using explicit warnings
            // rather than silently emitting empty `input_text` elements, which pollutes context.
            adk_core::types::Part::FileData { file_uri, .. } => {
                tracing::warn!(
                    "Dropping unsupported FileData part in OpenAI session: {}",
                    file_uri
                );
            }
            adk_core::types::Part::Thinking { .. } => {
                tracing::warn!("Dropping unsupported Thinking part in OpenAI session");
            }
            adk_core::types::Part::FunctionCall { name, .. } => {
                tracing::warn!(
                    "Dropping unsupported FunctionCall part in OpenAI session: {}",
                    name
                );
            }
            adk_core::types::Part::FunctionResponse { .. } => {
                tracing::warn!("Dropping unsupported FunctionResponse part in OpenAI session");
            }
            adk_core::types::Part::ServerToolCall { .. } => {
                tracing::warn!("Dropping unsupported ServerToolCall part in OpenAI session");
            }
            adk_core::types::Part::ServerToolResponse { .. } => {
                tracing::warn!("Dropping unsupported ServerToolResponse part in OpenAI session");
            }
        }
    }

    // 4. Wrap the translated payload in the exact JSON envelope required by the provider.
    json!({
        "type": "conversation.item.create",
        "item": {
            "type": "message",
            "role": openai_role,
            "content": content
        }
    })
}

/// Generate a random WebSocket key.
fn generate_ws_key() -> String {
    use base64::Engine;
    let mut key = [0u8; 16];
    getrandom::fill(&mut key).unwrap_or_default();
    base64::engine::general_purpose::STANDARD.encode(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::types::Part;

    #[test]
    fn test_openai_translate_text_only() {
        let parts = vec![Part::Text { text: "Hello".to_string() }];
        let value = translate_client_message("user", parts);

        let item = &value["item"];
        assert_eq!(item["role"], "user");

        let content = item["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(content[0]["text"], "Hello");
    }

    #[test]
    fn test_openai_translate_text_and_audio() {
        let parts = vec![
            Part::Text { text: "Listen:".to_string() },
            Part::InlineData { mime_type: "audio/wav".to_string(), data: vec![0x1, 0x2, 0x3] },
        ];
        let value = translate_client_message("user", parts);

        let content = value["item"]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);

        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(content[0]["text"], "Listen:");

        assert_eq!(content[1]["type"], "input_audio");
        assert_eq!(content[1]["audio"], "AQID"); // base64 encoded [1,2,3]
    }

    #[test]
    fn test_openai_skips_unsupported_parts() {
        let parts = vec![
            Part::Text { text: "First".to_string() },
            Part::InlineData { mime_type: "image/png".to_string(), data: vec![0x1] }, // Should be skipped because it's not audio
            Part::Thinking { thinking: "Hmm".to_string(), signature: None }, // Should be skipped
            Part::Text { text: "Last".to_string() },
        ];
        let value = translate_client_message("user", parts);

        let content = value["item"]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);

        assert_eq!(content[0]["text"], "First");
        assert_eq!(content[1]["text"], "Last");
    }
}

impl std::fmt::Debug for OpenAIRealtimeSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAIRealtimeSession")
            .field("session_id", &self.session_id)
            .field("connected", &self.connected.load(Ordering::SeqCst))
            .finish()
    }
}
