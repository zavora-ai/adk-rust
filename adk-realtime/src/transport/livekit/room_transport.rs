use crate::{
    audio::{AudioChunk, AudioFormat},
    error::{RealtimeError, Result},
    transport::{
        RealtimeMediaTransport,
        event::{TransportControl, TransportEvent},
    },
};
use futures_core::Stream;
use std::pin::Pin;

/// A media transport backed by a LiveKit room.
/// Currently a placeholder for the migration.
pub struct LiveKitRoomTransport {
    id: String,
    input_format: AudioFormat,
    output_format: AudioFormat,
}

impl LiveKitRoomTransport {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            input_format: AudioFormat::pcm16_24khz(),
            output_format: AudioFormat::pcm16_24khz(),
        }
    }
}

#[async_trait::async_trait]
impl RealtimeMediaTransport for LiveKitRoomTransport {
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
        // Placeholder
        Box::pin(tokio_stream::empty())
    }

    async fn send_audio(&self, _audio: AudioChunk) -> Result<()> {
        // Placeholder
        Ok(())
    }

    async fn send_control(&self, _control: TransportControl) -> Result<()> {
        // Placeholder
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        // Placeholder
        Ok(())
    }
}
