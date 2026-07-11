use adk_realtime::audio::{AudioChunk, AudioFormat, SmartAudioBuffer};
use adk_realtime::events::ToolResponse;
use adk_realtime::model::RealtimeModel;
use adk_realtime::runner::RealtimeRunner;
use adk_realtime::session::{ContextMutationOutcome, RealtimeSession};
use async_trait::async_trait;
use std::sync::Arc;

#[test]
fn test_pcm_byte_equivalence() {
    let samples = vec![i16::MIN, -1, 0, 1, i16::MAX];
    let format = AudioFormat::pcm16_24khz();
    let chunk = AudioChunk::from_i16_samples(&samples, format.clone());

    // Manual verification of little-endian bytes
    let expected_bytes = vec![
        0x00, 0x80, // i16::MIN (-32768)
        0xFF, 0xFF, // -1
        0x00, 0x00, // 0
        0x01, 0x00, // 1
        0xFF, 0x7F, // i16::MAX (32767)
    ];
    assert_eq!(chunk.data.as_ref(), &expected_bytes);

    let recovered = chunk.to_i16_samples().unwrap();
    assert_eq!(samples, recovered);
}

#[test]
fn test_misaligned_and_empty_bytes() {
    let format = AudioFormat::pcm16_24khz();

    // Empty
    let empty_chunk = AudioChunk::new(vec![], format.clone());
    assert!(empty_chunk.to_i16_samples().unwrap().is_empty());

    // Misaligned (odd length)
    let misaligned_chunk = AudioChunk::new(vec![0x01, 0x02, 0x03], format.clone());
    assert!(misaligned_chunk.to_i16_samples().is_err());
}

#[test]
fn test_smart_audio_buffer_retained_capacity() {
    let mut buffer = SmartAudioBuffer::new(16000, 40);
    let large_push = vec![0i16; 1600]; // 100ms
    buffer.push(&large_push);

    let initial_cap = buffer.capacity();
    assert!(initial_cap >= 1600);

    // Flush via process_and_clear
    buffer.process_and_clear(|_| {});
    assert_eq!(buffer.capacity(), initial_cap);
    assert_eq!(buffer.flush_remaining(), None);

    // Refill and check capacity again
    buffer.push(&large_push);
    assert_eq!(buffer.capacity(), initial_cap);
}

// ── Mock Session for Interruption Proof ────────────────────────────────

struct InterruptionMockSession {
    pub audio_cleared: Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait]
impl RealtimeSession for InterruptionMockSession {
    fn session_id(&self) -> &str {
        "mock"
    }
    fn is_connected(&self) -> bool {
        true
    }
    async fn send_audio(&self, _audio: &AudioChunk) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    async fn send_audio_base64(&self, _audio_base64: &str) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    async fn send_text(&self, _text: &str) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    async fn send_tool_response(&self, _response: ToolResponse) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    async fn commit_audio(&self) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    async fn clear_audio(&self) -> adk_realtime::error::Result<()> {
        self.audio_cleared.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    async fn create_response(&self) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    async fn interrupt(&self) -> adk_realtime::error::Result<()> {
        self.clear_audio().await
    }
    async fn send_event(
        &self,
        _event: adk_realtime::events::ClientEvent,
    ) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    async fn next_event(
        &self,
    ) -> Option<adk_realtime::error::Result<adk_realtime::events::ServerEvent>> {
        None
    }
    fn events(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn futures::Stream<
                    Item = adk_realtime::error::Result<adk_realtime::events::ServerEvent>,
                > + Send
                + '_,
        >,
    > {
        Box::pin(futures::stream::empty())
    }
    async fn close(&self) -> adk_realtime::error::Result<()> {
        Ok(())
    }
    async fn mutate_context(
        &self,
        _config: adk_realtime::config::RealtimeConfig,
    ) -> adk_realtime::error::Result<ContextMutationOutcome> {
        Ok(ContextMutationOutcome::Applied)
    }
}

struct MockModel {
    pub session: Arc<InterruptionMockSession>,
}

#[async_trait]
impl RealtimeModel for MockModel {
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
        _config: adk_realtime::config::RealtimeConfig,
    ) -> adk_realtime::error::Result<Box<dyn RealtimeSession>> {
        Ok(Box::new(InterruptionMockSession { audio_cleared: self.session.audio_cleared.clone() }))
    }
}

#[tokio::test]
async fn test_interruption_signals_clear() {
    let audio_cleared = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let session = Arc::new(InterruptionMockSession { audio_cleared: audio_cleared.clone() });
    let model = MockModel { session: session.clone() };

    let runner = RealtimeRunner::builder().model(Arc::new(model)).build().unwrap();
    runner.connect().await.unwrap();

    runner.interrupt().await.unwrap();
    assert!(audio_cleared.load(std::sync::atomic::Ordering::SeqCst));
}
