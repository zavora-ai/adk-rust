//! OpenAI Realtime session implementation.

use crate::config::RealtimeConfig;
use crate::error::{RealtimeError, Result};
use crate::events::ServerEvent;
use crate::openai::protocol::OpenAITransportLink;
use async_trait::async_trait;
use futures::{Sink, SinkExt, StreamExt};
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        Message,
        http::{Request, Uri},
    },
};

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsSource = futures::stream::SplitStream<WsStream>;

/// Outbound queue depth, in frames.
///
/// Matches `WRITER_CHANNEL_CAPACITY` in the Gemini session, which already uses this
/// pattern. Sized against realtime audio rather than throughput: at 20 ms frames this
/// is ~1.3 s of buffered speech, past which a producer should feel backpressure rather
/// than keep queueing audio that will be stale by the time it reaches the wire.
const OUTBOUND_CAPACITY: usize = 64;

/// How long [`OpenAITransportLink::close`] waits for the writer to finish before
/// abandoning it.
///
/// Teardown has to terminate. A peer that has stopped reading stalls the in-flight
/// write for as long as it likes, so an unbounded wait here would reintroduce the
/// hang the writer task exists to remove.
const CLOSE_GRACE: Duration = Duration::from_secs(5);

/// OpenAI Realtime session.
///
/// Manages a WebSocket connection to OpenAI's Realtime API.
pub struct OpenAIRealtimeSession {
    session_id: String,
    connected: Arc<AtomicBool>,
    outbound: mpsc::Sender<Message>,
    /// Signals the writer to stop draining the queue and close.
    ///
    /// Deliberately not the outbound queue: a close that queues behind the backlog is
    /// exactly as slow as the backlog, and a realtime session's queue is full in the
    /// ordinary case of a healthy-but-slow link, not only when the peer is dead.
    close: Mutex<Option<oneshot::Sender<()>>>,
    writer: Mutex<Option<JoinHandle<()>>>,
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

/// Owns the write half of the socket and drains `outbound` into it.
///
/// Sole owner by construction: no caller can hold a lock on the sink across the
/// network write, so no caller can block another — which is what lets `close` run
/// while a send is still in flight.
///
/// Stops on `close` (which preempts the queue), on a peer error, or when every sender
/// has been dropped. Writes a Close frame on the way out and marks the session
/// disconnected.
async fn writer_loop<S>(
    mut sink: S,
    mut outbound: mpsc::Receiver<Message>,
    mut close: oneshot::Receiver<()>,
    connected: Arc<AtomicBool>,
) where
    S: Sink<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    let mut peer_is_gone = false;

    loop {
        // `biased` so a pending close wins against a backlog that is still draining;
        // without it teardown is at the mercy of random branch order.
        let message = tokio::select! {
            biased;
            _ = &mut close => break,
            message = outbound.recv() => match message {
                Some(message) => message,
                None => break,
            },
        };

        if let Err(error) = sink.send(message).await {
            tracing::warn!(error = %error, "openai realtime write failed; connection is gone");
            peer_is_gone = true;
            break;
        }
    }

    // Best effort: a peer that already failed a write will not accept this either, and
    // a stalled peer never resolves it — which is why `shutdown_writer` bounds the wait
    // rather than trusting this to return.
    if !peer_is_gone {
        let _ = sink.send(Message::Close(None)).await;
    }

    connected.store(false, Ordering::SeqCst);
}

/// Signals the writer to close, then waits a bounded time for it to finish.
///
/// Split out from [`OpenAITransportLink::close`] so the bound can be tested against a
/// stalled peer without a live socket.
///
/// Returns an error when the grace period expires, so a caller that cares about a
/// clean shutdown can still tell the difference — `RealtimeRunner` logs it during
/// session resumption.
///
/// Aborting does **not** close the connection: `SplitSink` and `SplitStream` share the
/// stream through a `BiLock`, and this session keeps the read half, so the socket stays
/// open until the session itself is dropped. What the bound guarantees is only that
/// teardown returns to its caller.
async fn shutdown_writer(
    close: &Mutex<Option<oneshot::Sender<()>>>,
    writer: &Mutex<Option<JoinHandle<()>>>,
    grace: Duration,
) -> Result<()> {
    if let Some(signal) = close.lock().await.take() {
        let _ = signal.send(());
    }

    // Taken out of the mutex before awaiting: holding a lock across the grace period
    // would reintroduce, at five seconds, exactly the shape this change removes.
    let handle = writer.lock().await.take();

    if let Some(mut handle) = handle
        && tokio::time::timeout(grace, &mut handle).await.is_err()
    {
        tracing::warn!(
            grace_secs = grace.as_secs_f64(),
            "openai realtime writer did not finish; abandoning it"
        );
        handle.abort();
        return Err(RealtimeError::connection("Close timed out; writer abandoned"));
    }

    Ok(())
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

        let connected = Arc::new(AtomicBool::new(true));
        let (outbound, outbound_rx) = mpsc::channel(OUTBOUND_CAPACITY);
        let (close, close_rx) = oneshot::channel();
        let writer = tokio::spawn(writer_loop(sink, outbound_rx, close_rx, Arc::clone(&connected)));

        // Generate session ID (will be updated when we receive session.created)
        let session_id = uuid::Uuid::new_v4().to_string();

        let session = Self {
            session_id,
            connected,
            outbound,
            close: Mutex::new(Some(close)),
            writer: Mutex::new(Some(writer)),
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

        // Hands the frame to the writer task and returns. Applies backpressure only
        // when the queue is genuinely full, and never holds a lock across the network
        // write, so a slow peer cannot block an unrelated caller or teardown.
        self.outbound
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
        shutdown_writer(&self.close, &self.writer, CLOSE_GRACE).await
    }
}

impl Drop for OpenAIRealtimeSession {
    /// Stops the writer when the session is dropped without `close()`.
    ///
    /// A spawned task outlives the handle that is dropped rather than awaited, and a
    /// writer parked mid-send on a stalled peer has nothing left to wake it — it would
    /// hold the sink, the TLS session and the socket for the life of the process.
    /// Before this change there was no task to leak: dropping the session dropped both
    /// halves of the stream.
    ///
    /// `get_mut` rather than `try_lock`: `Drop` has exclusive access, so this cannot
    /// lose a race with a concurrent `close()`.
    fn drop(&mut self) {
        if let Some(handle) = self.writer.get_mut().take() {
            handle.abort();
        }
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
mod writer_tests {
    //! Teardown must not be blocked by a peer that stopped reading.
    //!
    //! The sink used to sit behind an `Arc<Mutex<SplitSink<..>>>` that both `send_raw`
    //! and `close` locked *across* their `.await` on the network write. Even with no
    //! concurrency at all, `close` then hung on its own write against a peer that had
    //! stopped draining — there was no timeout anywhere on the path. With a concurrent
    //! sender it was worse: `tokio::sync::Mutex` hands the lock out in order, so `close`
    //! also had to wait out an in-flight `send_raw` first.
    //!
    //! These tests pin what replaced it: teardown terminates and reports, close preempts
    //! a full queue rather than joining it, and the queue is bounded.

    use super::{CLOSE_GRACE, OUTBOUND_CAPACITY, shutdown_writer, writer_loop};
    use futures::Sink;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::{Context, Poll};
    use std::time::{Duration, Instant};
    use tokio::sync::{Mutex, mpsc, oneshot};
    use tokio_tungstenite::tungstenite::Message;

    /// A sink whose `send` never resolves — the peer that has stopped reading.
    ///
    /// It stalls at `poll_ready` where a real `SplitSink` stalls at `poll_flush`;
    /// `SinkExt::send` never resolves either way, which is the only property under test.
    struct StalledSink;

    impl Sink<Message> for StalledSink {
        type Error = std::io::Error;

        fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Pending
        }
        fn start_send(self: Pin<&mut Self>, _: Message) -> Result<(), Self::Error> {
            unreachable!("poll_ready never resolves, so start_send is never reached")
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Pending
        }
        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Pending
        }
    }

    /// A peer that accepts everything, recording what it was given.
    #[derive(Clone, Default)]
    struct RecordingSink(Arc<std::sync::Mutex<Vec<Message>>>);

    impl Sink<Message> for RecordingSink {
        type Error = std::io::Error;

        fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
        fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
            self.0.lock().unwrap().push(item);
            Ok(())
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    /// Spawns a writer over `sink` and returns the handles `shutdown_writer` needs.
    #[allow(clippy::type_complexity)]
    fn spawn_writer<S>(
        sink: S,
    ) -> (
        mpsc::Sender<Message>,
        Mutex<Option<oneshot::Sender<()>>>,
        Mutex<Option<tokio::task::JoinHandle<()>>>,
        Arc<AtomicBool>,
    )
    where
        S: Sink<Message> + Unpin + Send + 'static,
        S::Error: std::fmt::Display,
    {
        let connected = Arc::new(AtomicBool::new(true));
        let (outbound, outbound_rx) = mpsc::channel(OUTBOUND_CAPACITY);
        let (close, close_rx) = oneshot::channel();
        let writer = tokio::spawn(writer_loop(sink, outbound_rx, close_rx, Arc::clone(&connected)));
        (outbound, Mutex::new(Some(close)), Mutex::new(Some(writer)), connected)
    }

    /// The regression. Under the old code this call sat on the socket — and, with a
    /// concurrent sender, on the mutex — and never returned.
    ///
    /// Asserts more than "it did not hang": the grace period must actually elapse and
    /// the failure must be *reported*, so a `shutdown_writer` that silently did nothing
    /// fails this test.
    #[tokio::test]
    async fn teardown_reports_and_terminates_while_the_peer_is_still_stalled() {
        let (outbound, close, writer, _connected) = spawn_writer(StalledSink);

        // A frame the writer picks up and then blocks on forever.
        outbound.send(Message::Text("in flight".into())).await.expect("queued");
        tokio::task::yield_now().await;

        let grace = Duration::from_millis(200);
        let started = Instant::now();
        let outcome =
            tokio::time::timeout(Duration::from_secs(5), shutdown_writer(&close, &writer, grace))
                .await
                .expect("teardown must terminate even though the peer never drains");

        assert!(outcome.is_err(), "an abandoned writer must be reported, not swallowed");
        assert!(started.elapsed() >= grace, "the grace period must actually be honoured");
        assert!(writer.lock().await.is_none(), "the writer handle must be released");
    }

    /// Close must *preempt* the backlog, not queue behind it.
    ///
    /// A realtime queue is full in the ordinary case of a healthy-but-slow link, so a
    /// close carried by the outbound queue would be dropped or would wait out the whole
    /// backlog. The signal is separate, and `biased` select makes it win.
    #[tokio::test]
    async fn close_preempts_a_completely_full_queue() {
        let sink = RecordingSink::default();
        let (outbound, close, writer, _connected) = spawn_writer(sink.clone());

        // Fill the queue past capacity while the writer is not draining it yet.
        for frame in 0..OUTBOUND_CAPACITY {
            outbound
                .try_send(Message::Text(format!("frame {frame}").into()))
                .expect("queue accepts up to capacity");
        }
        assert!(
            outbound.try_send(Message::Text("overflow".into())).is_err(),
            "the queue must be bounded — an unbounded queue would hide the very stall \
             this design exists to survive"
        );

        tokio::time::timeout(Duration::from_secs(5), shutdown_writer(&close, &writer, CLOSE_GRACE))
            .await
            .expect("teardown must not wait out the backlog")
            .expect("a healthy peer must close cleanly");

        let written = sink.0.lock().unwrap().clone();
        assert!(
            matches!(written.last(), Some(Message::Close(_))),
            "the close frame must be written: {written:?}"
        );
        assert!(
            written.len() < OUTBOUND_CAPACITY,
            "close must preempt the backlog rather than drain all {OUTBOUND_CAPACITY} \
             queued frames first; wrote {}",
            written.len()
        );
    }

    /// The happy path: a healthy peer gets its Close frame, well inside the grace
    /// period, and the session is marked down.
    #[tokio::test]
    async fn a_healthy_peer_receives_the_close_frame_promptly() {
        let sink = RecordingSink::default();
        let (outbound, close, writer, connected) = spawn_writer(sink.clone());

        outbound.send(Message::Text("hello".into())).await.expect("queued");

        // Grace far exceeds the assertion window, so passing means teardown was prompt
        // rather than merely bounded.
        tokio::time::timeout(
            Duration::from_millis(500),
            shutdown_writer(&close, &writer, Duration::from_secs(30)),
        )
        .await
        .expect("a healthy peer must close promptly, not merely within the grace period")
        .expect("a healthy peer must close cleanly");

        let written = sink.0.lock().unwrap().clone();
        assert!(
            matches!(written.last(), Some(Message::Close(_))),
            "the close frame must reach a peer that is reading: {written:?}"
        );
        assert!(!connected.load(Ordering::SeqCst), "the writer marks the session disconnected");
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
