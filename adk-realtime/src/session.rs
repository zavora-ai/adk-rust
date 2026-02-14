//! Core RealtimeSession trait definition.

use crate::audio::AudioChunk;
use crate::error::Result;
use crate::events::{ClientEvent, ServerEvent, ToolResponse};
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

/// A real-time bidirectional streaming session.
///
/// This trait provides a unified interface for real-time voice/audio sessions
/// across different providers (OpenAI, Gemini, etc.).
///
/// # Example
///
/// ```rust,ignore
/// use adk_realtime::{RealtimeSession, ServerEvent};
///
/// async fn handle_session(session: &dyn RealtimeSession) -> Result<()> {
///     // Send audio
///     session.send_audio(audio_chunk).await?;
///
///     // Receive events
///     while let Some(event) = session.next_event().await {
///         match event? {
///             ServerEvent::AudioDelta { delta, .. } => { /* play audio */ }
///             ServerEvent::FunctionCallDone { name, arguments, call_id, .. } => {
///                 // Execute tool and respond
///                 let result = execute_tool(&name, &arguments);
///                 session.send_tool_response(call_id, result).await?;
///             }
///             _ => {}
///         }
///     }
///     Ok(())
/// }
/// ```
#[async_trait]
pub trait RealtimeSession: Send + Sync {
    /// Get the session ID.
    fn session_id(&self) -> &str;

    /// Check if the session is currently connected.
    fn is_connected(&self) -> bool;

    /// Send raw audio data to the server.
    ///
    /// The audio should be in the format specified in the session configuration.
    async fn send_audio(&self, audio: &AudioChunk) -> Result<()>;

    /// Send base64-encoded audio directly.
    async fn send_audio_base64(&self, audio_base64: &str) -> Result<()>;

    /// Send a text message.
    async fn send_text(&self, text: &str) -> Result<()>;

    /// Send a tool/function response.
    async fn send_tool_response(&self, response: ToolResponse) -> Result<()>;

    /// Commit the audio buffer (for manual VAD mode).
    async fn commit_audio(&self) -> Result<()>;

    /// Clear the audio input buffer.
    async fn clear_audio(&self) -> Result<()>;

    /// Trigger a response from the model.
    async fn create_response(&self) -> Result<()>;

    /// Interrupt/cancel the current response.
    async fn interrupt(&self) -> Result<()>;

    /// Send a raw client event.
    async fn send_event(&self, event: ClientEvent) -> Result<()>;

    /// Get the next event from the server.
    ///
    /// Returns `None` when the session is closed.
    async fn next_event(&self) -> Option<Result<ServerEvent>>;

    /// Get a stream of server events.
    fn events(&self) -> Pin<Box<dyn Stream<Item = Result<ServerEvent>> + Send + '_>>;

    /// Close the session gracefully.
    async fn close(&self) -> Result<()>;
}

/// Extension trait for RealtimeSession with convenience methods.
#[async_trait]
pub trait RealtimeSessionExt: RealtimeSession {
    /// Send audio and wait for the response to complete.
    async fn send_audio_and_wait(&self, audio: &AudioChunk) -> Result<Vec<ServerEvent>> {
        self.send_audio(audio).await?;
        self.commit_audio().await?;

        let mut events = Vec::new();
        while let Some(event) = self.next_event().await {
            let event = event?;
            let is_done = matches!(&event, ServerEvent::ResponseDone { .. });
            events.push(event);
            if is_done {
                break;
            }
        }
        Ok(events)
    }

    /// Send text and wait for the response to complete.
    async fn send_text_and_wait(&self, text: &str) -> Result<Vec<ServerEvent>> {
        self.send_text(text).await?;
        self.create_response().await?;

        let mut events = Vec::new();
        while let Some(event) = self.next_event().await {
            let event = event?;
            let is_done = matches!(&event, ServerEvent::ResponseDone { .. });
            events.push(event);
            if is_done {
                break;
            }
        }
        Ok(events)
    }

    /// Collect all audio chunks from a response (as raw bytes).
    async fn collect_audio(&self) -> Result<Vec<Vec<u8>>> {
        let mut audio_chunks = Vec::new();
        while let Some(event) = self.next_event().await {
            match event? {
                ServerEvent::AudioDelta { delta, .. } => {
                    audio_chunks.push(delta);
                }
                ServerEvent::ResponseDone { .. } => break,
                ServerEvent::Error { error, .. } => {
                    return Err(crate::error::RealtimeError::server(
                        error.code.unwrap_or_default(),
                        error.message,
                    ));
                }
                _ => {}
            }
        }
        Ok(audio_chunks)
    }
}

// Blanket implementation
impl<T: RealtimeSession> RealtimeSessionExt for T {}

/// A boxed session type for dynamic dispatch.
pub type BoxedSession = Box<dyn RealtimeSession>;
