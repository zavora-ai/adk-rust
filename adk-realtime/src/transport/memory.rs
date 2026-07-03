use super::{
    RealtimeMediaTransport,
    event::{TransportControl, TransportEvent},
};
use crate::{
    audio::{AudioChunk, AudioFormat},
    error::Result,
};
use futures_core::Stream;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

/// An in-memory transport for testing purposes.
pub struct InMemoryTransport {
    id: String,
    input_format: AudioFormat,
    output_format: AudioFormat,
    event_rx: Arc<Mutex<Option<mpsc::Receiver<Result<TransportEvent>>>>>,
    event_tx: mpsc::Sender<Result<TransportEvent>>,
    sent_audio: Arc<Mutex<Vec<AudioChunk>>>,
    sent_controls: Arc<Mutex<Vec<TransportControl>>>,
    is_closed: Arc<Mutex<bool>>,
}

impl InMemoryTransport {
    pub fn new(id: impl Into<String>) -> Self {
        let (tx, rx) = mpsc::channel(100);
        Self {
            id: id.into(),
            input_format: AudioFormat::pcm16_24khz(),
            output_format: AudioFormat::pcm16_24khz(),
            event_rx: Arc::new(Mutex::new(Some(rx))),
            event_tx: tx,
            sent_audio: Arc::new(Mutex::new(Vec::new())),
            sent_controls: Arc::new(Mutex::new(Vec::new())),
            is_closed: Arc::new(Mutex::new(false)),
        }
    }

    pub fn with_formats(mut self, input: AudioFormat, output: AudioFormat) -> Self {
        self.input_format = input;
        self.output_format = output;
        self
    }

    pub async fn push_event(&self, event: TransportEvent) -> Result<()> {
        self.event_tx
            .send(Ok(event))
            .await
            .map_err(|_| crate::error::RealtimeError::provider("Channel closed"))
    }

    pub async fn get_sent_audio(&self) -> Vec<AudioChunk> {
        self.sent_audio.lock().await.clone()
    }

    pub async fn get_sent_controls(&self) -> Vec<TransportControl> {
        self.sent_controls.lock().await.clone()
    }

    pub async fn is_closed(&self) -> bool {
        *self.is_closed.lock().await
    }
}

#[async_trait::async_trait]
impl RealtimeMediaTransport for InMemoryTransport {
    fn id(&self) -> &str {
        &self.id
    }

    fn input_format(&self) -> AudioFormat {
        self.input_format.clone()
    }

    fn output_format(&self) -> AudioFormat {
        self.output_format.clone()
    }

    fn events(&self) -> Pin<Box<dyn Stream<Item = Result<TransportEvent>> + Send + '_>> {
        let rx = self.event_rx.blocking_lock().take().expect("events() called multiple times");
        Box::pin(ReceiverStream::new(rx))
    }

    async fn send_audio(&self, audio: AudioChunk) -> Result<()> {
        if *self.is_closed.lock().await {
            return Err(crate::error::RealtimeError::provider("Transport closed"));
        }
        self.sent_audio.lock().await.push(audio);
        Ok(())
    }

    async fn send_control(&self, control: TransportControl) -> Result<()> {
        if *self.is_closed.lock().await {
            return Err(crate::error::RealtimeError::provider("Transport closed"));
        }
        self.sent_controls.lock().await.push(control);
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        *self.is_closed.lock().await = true;
        Ok(())
    }
}
