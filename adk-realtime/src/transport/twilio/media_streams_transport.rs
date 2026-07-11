use crate::{
    audio::{AudioChunk, AudioFormat},
    error::Result,
    transport::{
        RealtimeMediaTransport,
        event::{TransportControl, TransportEvent},
        twilio::serializer::TwilioMediaSerializer,
    },
};
use futures_core::Stream;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

/// A media transport backed by Twilio Media Streams.
pub struct TwilioMediaStreamsTransport {
    id: String,
    stream_sid: String,
    call_sid: String,
    tx: mpsc::Sender<String>,
    events_rx: Arc<Mutex<mpsc::Receiver<Result<TransportEvent>>>>,
}

impl TwilioMediaStreamsTransport {
    /// Create a new Twilio Media Streams transport.
    ///
    /// * `id` - Unique transport identifier.
    /// * `stream_sid` - Twilio StreamSid for this call segment.
    /// * `call_sid` - Twilio CallSid.
    /// * `tx` - Channel sender for outgoing JSON text messages.
    /// * `events_rx` - Channel receiver for incoming TransportEvents.
    pub fn new(
        id: impl Into<String>,
        stream_sid: impl Into<String>,
        call_sid: impl Into<String>,
        tx: mpsc::Sender<String>,
        events_rx: mpsc::Receiver<Result<TransportEvent>>,
    ) -> Self {
        Self {
            id: id.into(),
            stream_sid: stream_sid.into(),
            call_sid: call_sid.into(),
            tx,
            events_rx: Arc::new(Mutex::new(events_rx)),
        }
    }

    /// Get the Twilio CallSid.
    pub fn call_sid(&self) -> &str {
        &self.call_sid
    }

    /// Get the Twilio StreamSid.
    pub fn stream_sid(&self) -> &str {
        &self.stream_sid
    }
}

#[async_trait::async_trait]
impl RealtimeMediaTransport for TwilioMediaStreamsTransport {
    fn id(&self) -> &str {
        &self.id
    }

    fn input_format(&self) -> AudioFormat {
        // Twilio Media Streams ingress is raw mono G.711 μ-law at 8kHz.
        AudioFormat::g711_ulaw()
    }

    fn output_format(&self) -> AudioFormat {
        // Transport output expects PCM16 @ 24kHz (Gemini Live output format).
        AudioFormat::pcm16_24khz()
    }

    fn events(&self) -> Pin<Box<dyn Stream<Item = Result<TransportEvent>> + Send + '_>> {
        let rx = self.events_rx.clone();
        Box::pin(async_stream::stream! {
            let mut rx = rx.lock().await;
            while let Some(event) = rx.recv().await {
                yield event;
            }
        })
    }

    async fn send_audio(&self, audio: AudioChunk) -> Result<()> {
        let serializer = TwilioMediaSerializer::new();
        let msg = serializer.serialize_audio(&self.stream_sid, &audio);

        self.tx.send(msg).await.map_err(|e| {
            crate::error::RealtimeError::connection(format!(
                "Failed to send audio to Twilio: {}",
                e
            ))
        })?;

        Ok(())
    }

    async fn send_control(&self, control: TransportControl) -> Result<()> {
        let serializer = TwilioMediaSerializer::new();
        if let Some(msg) = serializer.serialize_control(&self.stream_sid, &control) {
            self.tx.send(msg).await.map_err(|e| {
                crate::error::RealtimeError::connection(format!(
                    "Failed to send control to Twilio: {}",
                    e
                ))
            })?;
        }
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertised_input_format_matches_parsed_media() {
        let (tx, _rx) = mpsc::channel(1);
        let (_events_tx, events_rx) = mpsc::channel(1);
        let transport =
            TwilioMediaStreamsTransport::new("twilio-test", "MZ123", "CA123", tx, events_rx);

        let message = r#"{
            "event": "media",
            "streamSid": "MZ123",
            "media": {
                "payload": "AA=="
            }
        }"#;

        let event = TwilioMediaSerializer::new().parse(message).unwrap().unwrap();
        let chunk = match event {
            TransportEvent::Audio { chunk, .. } => chunk,
            _ => panic!("expected Twilio media audio event"),
        };

        assert_eq!(transport.input_format(), AudioFormat::g711_ulaw());
        assert_eq!(transport.input_format(), chunk.format);
        assert_eq!(chunk.data.as_ref(), &[0x00]);
    }
}
