pub mod bridge;
pub mod config;
pub mod event;
pub mod memory;

#[cfg(feature = "livekit")]
pub mod livekit;

#[cfg(feature = "twilio")]
pub mod twilio;

use crate::error::Result;
use event::TransportEvent;
use futures_core::Stream;
use std::pin::Pin;

/// Trait representing a generic media transport for real-time voice sessions.
#[async_trait::async_trait]
pub trait RealtimeMediaTransport: Send + Sync {
    /// Returns the unique identifier for this transport instance.
    fn id(&self) -> &str;

    /// Returns the expected audio format for input (transport -> model).
    fn input_format(&self) -> crate::audio::AudioFormat;

    /// Returns the expected audio format for output (model -> transport).
    fn output_format(&self) -> crate::audio::AudioFormat;

    /// Returns a stream of events from the transport.
    fn events(&self) -> Pin<Box<dyn Stream<Item = Result<TransportEvent>> + Send + '_>>;

    /// Sends an audio chunk to the transport for playback.
    async fn send_audio(&self, audio: crate::audio::AudioChunk) -> Result<()>;

    /// Sends a control message to the transport.
    async fn send_control(&self, control: event::TransportControl) -> Result<()>;

    /// Closes the transport connection.
    async fn close(&self) -> Result<()>;
}
