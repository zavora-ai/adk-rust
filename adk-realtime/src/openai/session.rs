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

/// A short, stable-within-a-run digest of a frame.
///
/// Correlates repeated drift on the same shape without reproducing its content. Not a
/// cryptographic digest and not stable across processes; it exists to group log lines.
fn payload_digest(text: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// The frame itself, only when payload recording is compiled in.
///
/// Returns `"<redacted>"` otherwise, so the default build cannot leak conversation
/// content through a schema-drift warning.
fn redacted_payload(text: &str) -> String {
    if cfg!(feature = "record-payloads") {
        let end = text
            .char_indices()
            .map(|(index, _)| index)
            .chain(std::iter::once(text.len()))
            .take_while(|index| *index <= 300)
            .last()
            .unwrap_or(0);
        return text[..end].to_string();
    }
    "<redacted>".to_string()
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
                    Ok(event) => Some(Ok(event)),
                    Err(e) => {
                        // The type IS one we model but the fields didn't match —
                        // genuine schema drift worth surfacing.
                        // Realtime frames carry transcripts, tool arguments, tool
                        // results, and identifiers. Logging the raw frame put that
                        // content into warning logs at exactly the moment operators
                        // widen log collection — during provider schema drift. Log a
                        // field-safe summary instead, and record the frame only when
                        // payload recording is explicitly compiled in.
                        tracing::warn!(
                            event_type = %event_type,
                            error = %e,
                            payload.bytes = text.len(),
                            payload.digest = %payload_digest(&text),
                            payload.raw = %redacted_payload(&text),
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

#[cfg(test)]
mod redaction_tests {
    //! A schema-drift warning must not carry conversation content.
    //!
    //! The warning used to log the first 300 bytes of the raw WebSocket frame. Realtime
    //! frames carry transcripts, tool arguments, tool results, and identifiers, and the
    //! path was not gated on any payload-recording opt-in — so provider schema drift
    //! could push user content into warning logs at exactly the moment operators widen
    //! log collection.

    use super::{payload_digest, redacted_payload};

    /// Captures formatted log output so its content can be asserted.
    #[cfg(not(feature = "record-payloads"))]
    #[derive(Clone, Default)]
    struct CapturedLogs(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    #[cfg(not(feature = "record-payloads"))]
    impl std::io::Write for CapturedLogs {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[cfg(not(feature = "record-payloads"))]
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
        type Writer = Self;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// A frame shaped like a recognized event, holding a sentinel where a transcript
    /// would be.
    fn drifting_frame() -> String {
        serde_json::json!({
            "type": "conversation.item.input_audio_transcription.completed",
            "transcript": "my card is 4111-1111-1111-1111",
            "item_id": 12345
        })
        .to_string()
    }

    #[cfg(not(feature = "record-payloads"))]
    #[test]
    fn the_frame_is_withheld_by_default() {
        assert_eq!(redacted_payload(&drifting_frame()), "<redacted>");
    }

    #[cfg(feature = "record-payloads")]
    #[test]
    fn the_frame_is_recorded_when_explicitly_enabled() {
        // The opt-in exists so schema drift can still be diagnosed with the frame in
        // hand; it is a deliberate choice, not the default.
        let recorded = redacted_payload(&drifting_frame());
        assert!(recorded.contains("transcript"), "the frame must be recorded under the feature");
        assert!(recorded.len() <= 300, "recording stays bounded: {}", recorded.len());
    }

    #[cfg(not(feature = "record-payloads"))]
    #[test]
    fn a_drift_warning_logs_a_summary_and_no_content() {
        let frame = drifting_frame();
        let logs = CapturedLogs::default();
        let layer = tracing_subscriber::fmt::layer()
            .with_writer(logs.clone())
            .with_ansi(false)
            .with_target(false);
        let collector = {
            use tracing_subscriber::layer::SubscriberExt;
            tracing_subscriber::registry().with(layer)
        };

        tracing::subscriber::with_default(collector, || {
            tracing::warn!(
                event_type = "conversation.item.input_audio_transcription.completed",
                payload.bytes = frame.len(),
                payload.digest = %payload_digest(&frame),
                payload.raw = %redacted_payload(&frame),
                "recognized realtime event failed to parse (schema drift?)"
            );
        });

        let captured = String::from_utf8_lossy(&logs.0.lock().unwrap()).to_string();
        assert!(
            !captured.contains("4111-1111-1111-1111"),
            "the frame's content reached the log: {captured}"
        );
        assert!(captured.contains("<redacted>"), "the log must say the frame was withheld");
        assert!(captured.contains("payload.bytes"), "the size must remain available");
        assert!(captured.contains("payload.digest"), "a digest must remain available");
    }

    #[test]
    fn the_digest_correlates_repeats_and_separates_shapes() {
        let frame = r#"{"type":"response.done","response":{"unexpected":true}}"#;
        assert_eq!(payload_digest(frame), payload_digest(frame));
        assert_ne!(payload_digest(frame), payload_digest(r#"{"type":"response.done"}"#));
    }
}
