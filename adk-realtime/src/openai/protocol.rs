use crate::RealtimeSession;
use crate::audio::AudioChunk;
use crate::error::{RealtimeError, Result};
use crate::events::{ClientEvent, ServerEvent, ToolResponse};
use crate::session::ContextMutationOutcome;
use async_trait::async_trait;
use futures::Stream;
use serde_json::{Value, json};
use std::pin::Pin;

/// A minimal transport trait abstracting WebSocket, WebRTC, etc.
#[async_trait]
pub trait OpenAITransportLink: Send + Sync {
    /// Provide the unique session id.
    fn session_id(&self) -> &str;

    /// Is the transport currently connected and healthy?
    fn is_connected(&self) -> bool;

    /// Send a raw JSON payload to the provider.
    async fn send_raw(&self, payload: &Value) -> Result<()>;

    /// Read the next parsed ServerEvent from the provider.
    async fn receive_raw(&self) -> Option<Result<ServerEvent>>;

    /// Gracefully terminate the connection.
    async fn close(&self) -> Result<()>;

    /// Send PCM16 audio. For WebSocket, defaults to base64 encoding over `send_raw`.
    /// For WebRTC, this is MUST be overridden to directly write to the media track.
    async fn send_audio(&self, audio: &crate::audio::AudioChunk) -> Result<()> {
        self.send_audio_base64(&audio.to_base64()).await
    }

    /// Send base64-encoded PCM16 audio. Defaults to `input_audio_buffer.append` via `send_raw`.
    /// For WebRTC, this MUST be overridden to directly decode and write to the media track.
    async fn send_audio_base64(&self, audio_base64: &str) -> Result<()> {
        let event = json!({
            "type": "input_audio_buffer.append",
            "audio": audio_base64
        });
        self.send_raw(&event).await
    }

    /// Trigger a specific native configuration logic payload if needed by the transport
    /// Note: mostly WebRTC uses configure_session dynamically over the data channel
    async fn configure_session(&self, config: crate::config::RealtimeConfig) -> Result<()> {
        // By default sending a standard update
        let update_json = convert_config_to_openai(&config);
        let event = json!({
            "type": "session.update",
            "session": update_json
        });
        self.send_raw(&event).await
    }
}

/// Convert this configuration to an OpenAI specific session configuration.
///
/// This follows the schema expected by the `session.update` client event
/// for both WebSocket and WebRTC transports.
pub(crate) fn convert_config_to_openai(config: &crate::config::RealtimeConfig) -> Value {
    use crate::config::VadMode;

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
            VadMode::ServerVad => {
                let mut cfg = json!({ "type": "server_vad" });
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
            VadMode::SemanticVad => {
                let mut cfg = json!({ "type": "semantic_vad" });
                if let Some(eagerness) = &vad.eagerness {
                    cfg["eagerness"] = json!(eagerness);
                }
                cfg
            }
            VadMode::None => {
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

    session_config
}

/// The universal Protocol Handler wrapping any transport layer.
pub struct OpenAIProtocolHandler<T: OpenAITransportLink> {
    pub transport: T,
}

impl<T: OpenAITransportLink> OpenAIProtocolHandler<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }
}

#[async_trait]
impl<T: OpenAITransportLink> RealtimeSession for OpenAIProtocolHandler<T> {
    fn session_id(&self) -> &str {
        self.transport.session_id()
    }

    fn is_connected(&self) -> bool {
        self.transport.is_connected()
    }

    async fn send_audio(&self, audio: &AudioChunk) -> Result<()> {
        self.transport.send_audio(audio).await
    }

    async fn send_audio_base64(&self, audio_base64: &str) -> Result<()> {
        self.transport.send_audio_base64(audio_base64).await
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
        self.transport.send_raw(&event).await
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
        self.transport.send_raw(&event).await?;

        // Trigger response after tool output
        self.create_response().await
    }

    async fn commit_audio(&self) -> Result<()> {
        let event = json!({ "type": "input_audio_buffer.commit" });
        self.transport.send_raw(&event).await
    }

    async fn clear_audio(&self) -> Result<()> {
        let event = json!({ "type": "input_audio_buffer.clear" });
        self.transport.send_raw(&event).await
    }

    async fn create_response(&self) -> Result<()> {
        let event = json!({ "type": "response.create" });
        self.transport.send_raw(&event).await
    }

    async fn interrupt(&self) -> Result<()> {
        let event = json!({ "type": "response.cancel" });
        self.transport.send_raw(&event).await
    }

    async fn send_event(&self, event: ClientEvent) -> Result<()> {
        match event {
            ClientEvent::Message { role, parts } => {
                let payload = translate_client_message(&role, parts);
                tracing::info!(role = ?role, "injecting mid-flight context via native adk-rust types");
                self.transport.send_raw(&payload).await
            }
            ClientEvent::UpdateSession { .. } => {
                tracing::error!(
                    "internal UpdateSession intent leaked to the OpenAI transport socket"
                );
                Err(RealtimeError::ProviderError("Internal intent leaked to transport".to_string()))
            }
            other => {
                let value = serde_json::to_value(&other)
                    .map_err(|e| RealtimeError::protocol(format!("serialize error: {e}")))?;
                self.transport.send_raw(&value).await
            }
        }
    }

    async fn next_event(&self) -> Option<Result<ServerEvent>> {
        self.transport.receive_raw().await
    }

    fn events(&self) -> Pin<Box<dyn Stream<Item = Result<ServerEvent>> + Send + '_>> {
        Box::pin(futures::stream::unfold(self, |session| async move {
            let event = session.transport.receive_raw().await?;
            Some((event, session))
        }))
    }

    async fn close(&self) -> Result<()> {
        self.transport.close().await
    }

    async fn mutate_context(
        &self,
        config: crate::config::RealtimeConfig,
    ) -> Result<ContextMutationOutcome> {
        tracing::info!("updating OpenAI realtime session context via unified transport handler");
        self.transport.configure_session(config).await?;
        Ok(ContextMutationOutcome::Applied)
    }
}

/// Pure translation function for converting a standard `adk_core` message into
/// OpenAI Realtime API's native `conversation.item.create` payload.
pub(crate) fn translate_client_message(role: &str, parts: Vec<adk_core::types::Part>) -> Value {
    let openai_role = match role {
        "system" | "developer" => "system",
        "user" => "user",
        "model" | "assistant" => "assistant",
        _ => "user",
    };

    let mut content: Vec<Value> = Vec::new();
    for p in parts {
        match p {
            adk_core::types::Part::Text { text } => {
                content.push(json!({ "type": "input_text", "text": text }));
            }
            adk_core::types::Part::InlineData { mime_type, data } => {
                if mime_type.starts_with("audio/") {
                    use base64::Engine;
                    let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
                    content.push(json!({ "type": "input_audio", "audio": encoded }));
                } else {
                    tracing::warn!(
                        "dropping unsupported InlineData (non-audio) part in OpenAI session: {mime_type}"
                    );
                }
            }
            adk_core::types::Part::FileData { file_uri, .. } => {
                tracing::warn!("dropping unsupported FileData part in OpenAI session: {file_uri}");
            }
            adk_core::types::Part::Thinking { .. } => {
                tracing::warn!("dropping unsupported Thinking part in OpenAI session");
            }
            adk_core::types::Part::FunctionCall { name, .. } => {
                tracing::warn!("dropping unsupported FunctionCall part in OpenAI session: {name}");
            }
            adk_core::types::Part::FunctionResponse { .. } => {
                tracing::warn!("dropping unsupported FunctionResponse part in OpenAI session");
            }
            adk_core::types::Part::ServerToolCall { .. } => {
                tracing::warn!("dropping unsupported ServerToolCall part in OpenAI session");
            }
            adk_core::types::Part::ServerToolResponse { .. } => {
                tracing::warn!("dropping unsupported ServerToolResponse part in OpenAI session");
            }
        }
    }

    json!({
        "type": "conversation.item.create",
        "item": {
            "type": "message",
            "role": openai_role,
            "content": content
        }
    })
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
        assert_eq!(content[1]["audio"], "AQID");
    }

    #[test]
    fn test_openai_skips_unsupported_parts() {
        let parts = vec![
            Part::Text { text: "First".to_string() },
            Part::InlineData { mime_type: "image/png".to_string(), data: vec![0x1] },
            Part::Thinking { thinking: "Hmm".to_string(), signature: None },
            Part::Text { text: "Last".to_string() },
        ];
        let value = translate_client_message("user", parts);
        let content = value["item"]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["text"], "First");
        assert_eq!(content[1]["text"], "Last");
    }
}
