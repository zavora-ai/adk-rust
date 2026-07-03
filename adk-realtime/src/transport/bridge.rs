use crate::{
    error::Result,
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
        let runner_for_transport = self.runner.clone();
        let transport_for_model = self.transport.clone();

        let t2m_handle = tokio::spawn(async move {
            Self::pump_transport_to_model(transport_for_model, runner_for_transport).await
        });

        // The model to transport side is more complex as it would need to hook into the runner's event stream,
        // which isn't cleanly exposed for multiple consumers right now without `run()`.
        // Leaving as a placeholder for the future full implementation.
        let m2t_handle = tokio::spawn(async move { Ok(()) });

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
}
