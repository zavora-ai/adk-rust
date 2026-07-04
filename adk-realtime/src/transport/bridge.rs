use crate::{
    audio::AudioChunk,
    error::Result,
    events::ServerEvent,
    runner::RealtimeRunner,
    transport::{RealtimeMediaTransport, event::TransportEvent},
};

use futures_util::StreamExt;
use std::sync::Arc;
use tokio::task::JoinHandle;

/// Core bridge connecting a media transport with a realtime model session.
pub struct RealtimeTransportBridge {
    transport: Arc<dyn RealtimeMediaTransport>,
    runner: Arc<RealtimeRunner>,
}

impl RealtimeTransportBridge {
    pub fn new(transport: Arc<dyn RealtimeMediaTransport>, runner: Arc<RealtimeRunner>) -> Self {
        Self { transport, runner }
    }

    /// Spawns background tasks to pump data between transport and model.
    /// Does not block. Returns the join handles for both tasks.
    pub fn spawn_pump_tasks(&self) -> (JoinHandle<Result<()>>, JoinHandle<Result<()>>) {
        let runner_for_t2m = self.runner.clone();
        let transport_for_t2m = self.transport.clone();

        let t2m_handle = tokio::spawn(async move {
            Self::pump_transport_to_model(transport_for_t2m, runner_for_t2m).await
        });

        let runner_for_m2t = self.runner.clone();
        let transport_for_m2t = self.transport.clone();
        let m2t_handle = tokio::spawn(async move {
            Self::pump_model_to_transport(transport_for_m2t, runner_for_m2t).await
        });

        (t2m_handle, m2t_handle)
    }

    pub async fn pump_transport_to_model(
        transport: Arc<dyn RealtimeMediaTransport>,
        runner: Arc<RealtimeRunner>,
    ) -> Result<()> {
        let mut events = transport.events();

        while let Some(event_result) = events.next().await {
            let event = event_result?;
            match event {
                TransportEvent::Audio { chunk, .. } => {
                    runner.send_audio(&chunk.to_base64()).await?;
                }
                TransportEvent::Dtmf { .. } => {
                    // Application event, not model text unless explicitly mapped
                }
                TransportEvent::Interrupted => {
                    runner.interrupt().await?;
                }
                TransportEvent::Stopped { .. } | TransportEvent::Error { .. } => {
                    break;
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub async fn pump_model_to_transport(
        transport: Arc<dyn RealtimeMediaTransport>,
        runner: Arc<RealtimeRunner>,
    ) -> Result<()> {
        while let Some(event_result) = runner.next_event().await {
            let event = event_result?;
            match event {
                ServerEvent::AudioDelta { delta, .. } => {
                    // # Format contract
                    //
                    // Both Gemini Live and OpenAI Realtime emit audio deltas as raw
                    // PCM16 @ 24 kHz, regardless of what the downstream transport
                    // expects.  The `AudioChunk` label must therefore always reflect
                    // the *model's* native output format — not `transport.output_format()`.
                    //
                    // The transport implementation is responsible for resampling and/or
                    // transcoding to its own output format.  If we labelled the chunk
                    // with the transport's format the transport would skip resampling
                    // (assuming no conversion is needed), producing garbled audio.
                    //
                    // # Supporting additional providers
                    //
                    // When integrating a provider whose native format differs from
                    // PCM16 @ 24 kHz, expose a `native_audio_output_format()` method
                    // on `RealtimeRunner` (or the underlying session) and replace the
                    // hardcoded `pcm16_24khz()` constant with that value here.
                    let chunk = AudioChunk::new(delta, crate::audio::AudioFormat::pcm16_24khz());
                    transport.send_audio(chunk).await?;
                }
                ServerEvent::ResponseDone { .. } => {
                    // Could send marks or flush here
                }
                ServerEvent::Error { error, .. } => {
                    tracing::error!("Model error received in pump: {:?}", error);
                    // `ErrorInfo` has no blanket `Into<RealtimeError>` impl.
                    // Use the explicit two-argument constructor (same pattern as runner.rs).
                    return Err(crate::error::RealtimeError::server(
                        error.code.unwrap_or_default(),
                        error.message,
                    ));
                }
                _ => {}
            }
        }
        Ok(())
    }
}
