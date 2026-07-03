use adk_realtime::{
    audio::{AudioChunk, AudioFormat},
    config::RealtimeConfig,
    events::{ClientEvent, ServerEvent, ToolResponse},
    runner::{EventHandler, RealtimeRunner},
    session::RealtimeSession,
    transport::{
        bridge::RealtimeTransportBridge, event::TransportEvent, memory::InMemoryTransport,
    },
};
use async_trait::async_trait;
use futures_core::stream::Stream;
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::time::Duration;

struct MockEventHandler;
#[async_trait]
impl EventHandler for MockEventHandler {
    async fn on_audio(&self, _audio: &[u8], _item_id: &str) -> adk_realtime::error::Result<()> {
        Ok(())
    }
}

struct TrackingSession {
    audio_count: Arc<AtomicUsize>,
}
#[async_trait]
impl RealtimeSession for TrackingSession {
    fn is_connected(&self) -> bool {
        true
    }
    fn session_id(&self) -> &str {
        "tracker"
    }
    async fn send_audio(&self, _audio: &AudioChunk) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    async fn interrupt(&self) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    fn events(
        &self,
    ) -> Pin<Box<dyn Stream<Item = adk_realtime::error::Result<ServerEvent>> + Send + '_>> {
        let (_tx, rx) = mpsc::channel(1);
        Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
    }
    async fn next_event(&self) -> Option<adk_realtime::error::Result<ServerEvent>> {
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        None
    }
    async fn send_event(&self, _event: ClientEvent) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    async fn send_audio_base64(&self, _audio_base64: &str) -> adk_realtime::error::Result<()> {
        self.audio_count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn send_text(&self, _text: &str) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    async fn send_video_frame(
        &self,
        _mime_type: &str,
        _data_base64: &str,
    ) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    async fn commit_audio(&self) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    async fn clear_audio(&self) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    async fn create_response(&self) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    async fn send_tool_response(&self, _response: ToolResponse) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    async fn close(&self) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    async fn mutate_context(
        &self,
        _config: RealtimeConfig,
    ) -> adk_realtime::error::Result<adk_realtime::session::ContextMutationOutcome> {
        Ok(adk_realtime::session::ContextMutationOutcome::Applied)
    }
}

struct DummyModelT2M {
    session: Arc<TrackingSession>,
}
#[async_trait]
impl adk_realtime::model::RealtimeModel for DummyModelT2M {
    fn provider(&self) -> &str {
        "mock"
    }
    fn model_id(&self) -> &str {
        "mock"
    }
    fn supported_input_formats(&self) -> Vec<AudioFormat> {
        vec![]
    }
    fn supported_output_formats(&self) -> Vec<AudioFormat> {
        vec![]
    }
    fn available_voices(&self) -> Vec<&str> {
        vec![]
    }
    async fn connect(
        &self,
        _config: RealtimeConfig,
    ) -> adk_realtime::error::Result<Box<dyn RealtimeSession>> {
        let s = self.session.clone();
        Ok(Box::new(DummySessionWrapT2M(s)))
    }
}
struct DummySessionWrapT2M(Arc<TrackingSession>);
#[async_trait]
impl RealtimeSession for DummySessionWrapT2M {
    fn is_connected(&self) -> bool {
        true
    }
    fn session_id(&self) -> &str {
        self.0.session_id()
    }
    async fn send_audio(&self, audio: &AudioChunk) -> adk_realtime::error::Result<()> {
        self.0.send_audio(audio).await
    }
    async fn interrupt(&self) -> adk_realtime::error::Result<()> {
        self.0.interrupt().await
    }
    fn events(
        &self,
    ) -> Pin<Box<dyn Stream<Item = adk_realtime::error::Result<ServerEvent>> + Send + '_>> {
        let (_tx, rx) = mpsc::channel(1);
        Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
    }
    async fn next_event(&self) -> Option<adk_realtime::error::Result<ServerEvent>> {
        self.0.next_event().await
    }
    async fn send_event(&self, event: ClientEvent) -> adk_realtime::error::Result<()> {
        self.0.send_event(event).await
    }
    async fn send_audio_base64(&self, audio_base64: &str) -> adk_realtime::error::Result<()> {
        self.0.send_audio_base64(audio_base64).await
    }
    async fn send_text(&self, text: &str) -> adk_realtime::error::Result<()> {
        self.0.send_text(text).await
    }
    async fn send_video_frame(
        &self,
        mime_type: &str,
        data_base64: &str,
    ) -> adk_realtime::error::Result<()> {
        self.0.send_video_frame(mime_type, data_base64).await
    }
    async fn commit_audio(&self) -> adk_realtime::error::Result<()> {
        self.0.commit_audio().await
    }
    async fn clear_audio(&self) -> adk_realtime::error::Result<()> {
        self.0.clear_audio().await
    }
    async fn create_response(&self) -> adk_realtime::error::Result<()> {
        self.0.create_response().await
    }
    async fn send_tool_response(&self, response: ToolResponse) -> adk_realtime::error::Result<()> {
        self.0.send_tool_response(response).await
    }
    async fn close(&self) -> adk_realtime::error::Result<()> {
        self.0.close().await
    }
    async fn mutate_context(
        &self,
        config: RealtimeConfig,
    ) -> adk_realtime::error::Result<adk_realtime::session::ContextMutationOutcome> {
        self.0.mutate_context(config).await
    }
}

#[tokio::test]
async fn test_transport_to_model() {
    let transport = Arc::new(InMemoryTransport::new("test"));
    let audio_count = Arc::new(AtomicUsize::new(0));
    let session = Arc::new(TrackingSession { audio_count: audio_count.clone() });
    let model = Arc::new(DummyModelT2M { session });

    let runner =
        RealtimeRunner::builder().model(model).event_handler(MockEventHandler).build().unwrap();
    runner.connect().await.unwrap();

    let bridge = RealtimeTransportBridge::new(transport.clone(), Arc::new(runner));
    let (t2m_handle, _m2t_handle) = bridge.spawn_pump_tasks();

    transport
        .push_event(TransportEvent::Audio {
            chunk: AudioChunk::pcm16_24khz(vec![1, 2, 3, 4]),
            timestamp_ms: None,
            sequence: None,
            source: None,
        })
        .await
        .unwrap();

    transport.push_event(TransportEvent::Stopped { reason: None }).await.unwrap();

    let _: () = t2m_handle.await.unwrap().unwrap();
    assert_eq!(audio_count.load(Ordering::SeqCst), 1);
}

struct TriggerSession {
    events: Arc<Mutex<Vec<ServerEvent>>>,
}
#[async_trait]
impl RealtimeSession for TriggerSession {
    fn is_connected(&self) -> bool {
        true
    }
    fn session_id(&self) -> &str {
        "trigger"
    }
    async fn send_audio(&self, _audio: &AudioChunk) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    async fn interrupt(&self) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    fn events(
        &self,
    ) -> Pin<Box<dyn Stream<Item = adk_realtime::error::Result<ServerEvent>> + Send + '_>> {
        let (_tx, rx) = mpsc::channel(1);
        Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
    }
    async fn next_event(&self) -> Option<adk_realtime::error::Result<ServerEvent>> {
        let mut guard = self.events.lock().await;
        if guard.is_empty() {
            Some(Ok(ServerEvent::Error {
                event_id: "1".into(),
                error: adk_realtime::events::ErrorInfo {
                    error_type: "disconnect".into(),
                    code: None,
                    message: "stop".into(),
                    param: None,
                },
            }))
        } else {
            Some(Ok(guard.remove(0)))
        }
    }
    async fn send_event(&self, _event: ClientEvent) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    async fn send_audio_base64(&self, _audio_base64: &str) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    async fn send_text(&self, _text: &str) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    async fn send_video_frame(
        &self,
        _mime_type: &str,
        _data_base64: &str,
    ) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    async fn commit_audio(&self) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    async fn clear_audio(&self) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    async fn create_response(&self) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    async fn send_tool_response(&self, _response: ToolResponse) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    async fn close(&self) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    async fn mutate_context(
        &self,
        _config: RealtimeConfig,
    ) -> adk_realtime::error::Result<adk_realtime::session::ContextMutationOutcome> {
        Ok(adk_realtime::session::ContextMutationOutcome::Applied)
    }
}

struct DummyModelM2T {
    session: Arc<TriggerSession>,
}
#[async_trait]
impl adk_realtime::model::RealtimeModel for DummyModelM2T {
    fn provider(&self) -> &str {
        "mock"
    }
    fn model_id(&self) -> &str {
        "mock"
    }
    fn supported_input_formats(&self) -> Vec<AudioFormat> {
        vec![]
    }
    fn supported_output_formats(&self) -> Vec<AudioFormat> {
        vec![]
    }
    fn available_voices(&self) -> Vec<&str> {
        vec![]
    }
    async fn connect(
        &self,
        _config: RealtimeConfig,
    ) -> adk_realtime::error::Result<Box<dyn RealtimeSession>> {
        let s = self.session.clone();
        Ok(Box::new(DummySessionWrapM2T(s)))
    }
}
struct DummySessionWrapM2T(Arc<TriggerSession>);
#[async_trait]
impl RealtimeSession for DummySessionWrapM2T {
    fn is_connected(&self) -> bool {
        true
    }
    fn session_id(&self) -> &str {
        self.0.session_id()
    }
    async fn send_audio(&self, audio: &AudioChunk) -> adk_realtime::error::Result<()> {
        self.0.send_audio(audio).await
    }
    async fn interrupt(&self) -> adk_realtime::error::Result<()> {
        self.0.interrupt().await
    }
    fn events(
        &self,
    ) -> Pin<Box<dyn Stream<Item = adk_realtime::error::Result<ServerEvent>> + Send + '_>> {
        let (_tx, rx) = mpsc::channel(1);
        Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
    }
    async fn next_event(&self) -> Option<adk_realtime::error::Result<ServerEvent>> {
        self.0.next_event().await
    }
    async fn send_event(&self, event: ClientEvent) -> adk_realtime::error::Result<()> {
        self.0.send_event(event).await
    }
    async fn send_audio_base64(&self, audio_base64: &str) -> adk_realtime::error::Result<()> {
        self.0.send_audio_base64(audio_base64).await
    }
    async fn send_text(&self, text: &str) -> adk_realtime::error::Result<()> {
        self.0.send_text(text).await
    }
    async fn send_video_frame(
        &self,
        mime_type: &str,
        data_base64: &str,
    ) -> adk_realtime::error::Result<()> {
        self.0.send_video_frame(mime_type, data_base64).await
    }
    async fn commit_audio(&self) -> adk_realtime::error::Result<()> {
        self.0.commit_audio().await
    }
    async fn clear_audio(&self) -> adk_realtime::error::Result<()> {
        self.0.clear_audio().await
    }
    async fn create_response(&self) -> adk_realtime::error::Result<()> {
        self.0.create_response().await
    }
    async fn send_tool_response(&self, response: ToolResponse) -> adk_realtime::error::Result<()> {
        self.0.send_tool_response(response).await
    }
    async fn close(&self) -> adk_realtime::error::Result<()> {
        self.0.close().await
    }
    async fn mutate_context(
        &self,
        config: RealtimeConfig,
    ) -> adk_realtime::error::Result<adk_realtime::session::ContextMutationOutcome> {
        self.0.mutate_context(config).await
    }
}

#[tokio::test]
async fn test_model_to_transport() {
    let transport = Arc::new(InMemoryTransport::new("test"));
    let events = vec![ServerEvent::AudioDelta {
        event_id: "ev1".into(),
        response_id: "res1".into(),
        item_id: "item1".into(),
        output_index: 0,
        content_index: 0,
        delta: vec![10, 20, 30],
    }];
    let session = Arc::new(TriggerSession { events: Arc::new(Mutex::new(events)) });
    let model = Arc::new(DummyModelM2T { session });

    let runner =
        RealtimeRunner::builder().model(model).event_handler(MockEventHandler).build().unwrap();
    runner.connect().await.unwrap();

    let runner_arc = Arc::new(runner);
    let _ = tokio::time::timeout(
        Duration::from_millis(50),
        RealtimeTransportBridge::pump_model_to_transport(transport.clone(), runner_arc),
    )
    .await;

    let sent_audio = transport.get_sent_audio().await;
    assert_eq!(sent_audio.len(), 1);
    assert_eq!(sent_audio[0].data, vec![10, 20, 30]);
}
