//! OpenAI Realtime session implementation.

use crate::config::RealtimeConfig;
use crate::error::{RealtimeError, Result};
use crate::events::ServerEvent;
use crate::openai::protocol::OpenAITransportLink;
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use serde_json::Value;
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

        // Send initial session configuration via the trait default implementation
        session.configure_session(config).await?;

        Ok(session)
    }
}

#[async_trait]
impl OpenAITransportLink for OpenAIRealtimeSession {
    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

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

    async fn receive_raw(&self) -> Option<Result<ServerEvent>> {
        let mut receiver = self.receiver.lock().await;

        match receiver.next().await {
            Some(Ok(Message::Text(text))) => {
                // Extract the event type for logging
                let event_type = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(String::from))
                    .unwrap_or_else(|| "unknown".to_string());

                match serde_json::from_str::<ServerEvent>(&text) {
                    Ok(ServerEvent::Unknown) => {
                        // An event type we don't model. This is expected — the GA
                        // API emits many lifecycle events (conversation.item.*,
                        // response.content_part.*, rate_limits, …) that consumers
                        // don't need. Forward-compat by design, so debug-level.
                        tracing::debug!(
                            event_type = %event_type,
                            "unmodeled realtime event, ignored"
                        );
                        Some(Ok(ServerEvent::Unknown))
                    }
                    Ok(mut event) => {
                        // Normalize FunctionCallDone arguments: OpenAI sends them as a JSON-encoded string.
                        if let ServerEvent::FunctionCallDone { arguments, name, .. } = &mut event
                            && let serde_json::Value::String(s) = arguments
                        {
                            match serde_json::from_str::<serde_json::Value>(s) {
                                Ok(parsed) => {
                                    *arguments = parsed;
                                }
                                Err(e) => {
                                    tracing::error!(
                                        name = %name,
                                        error = %e,
                                        "failed to parse OpenAI function arguments as JSON"
                                    );
                                    return Some(Err(RealtimeError::protocol(format!(
                                        "malformed function arguments for {}: {}",
                                        name, e
                                    ))));
                                }
                            }
                        }
                        Some(Ok(event))
                    }
                    Err(e) => {
                        // The type IS one we model but the fields didn't match —
                        // genuine schema drift worth surfacing.
                        tracing::warn!(
                            event_type = %event_type,
                            error = %e,
                            raw = &text[..text.len().min(300)],
                            "recognized realtime event failed to parse (schema drift?)"
                        );
                        Some(Ok(ServerEvent::Unknown))
                    }
                }
            }
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

/// Generate a random WebSocket key.
fn generate_ws_key() -> String {
    use base64::Engine;
    let mut key = [0u8; 16];
    getrandom::fill(&mut key).unwrap_or_default();
    base64::engine::general_purpose::STANDARD.encode(key)
}

impl std::fmt::Debug for OpenAIRealtimeSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAIRealtimeSession")
            .field("session_id", &self.session_id)
            .field("connected", &self.connected.load(Ordering::SeqCst))
            .finish()
    }
}
