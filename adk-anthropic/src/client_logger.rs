//! Logging trait for Anthropic client operations.
//!
//! This module provides the [`ClientLogger`] trait that allows users to capture
//! and log all API interactions passing through the [`Anthropic`] client.

use crate::{Message, MessageStreamEvent};

/// A trait for logging Anthropic client operations.
///
/// Implement this trait to capture and record all API interactions,
/// including both non-streaming responses and individual streaming events.
///
/// # Example
///
/// ```rust,ignore
/// use adk_anthropic::{ClientLogger, Message, MessageStreamEvent};
/// use std::sync::Mutex;
///
/// struct FileLogger {
///     file: Mutex<std::fs::File>,
/// }
///
/// impl ClientLogger for FileLogger {
///     fn log_response(&self, message: &Message) {
///         let mut file = self.file.lock().unwrap();
///         writeln!(file, "Response: {}", serde_json::to_string(message).unwrap()).unwrap();
///     }
///
///     fn log_stream_event(&self, event: &MessageStreamEvent) {
///         let mut file = self.file.lock().unwrap();
///         writeln!(file, "Stream event: {}", serde_json::to_string(event).unwrap()).unwrap();
///     }
///
///     fn log_stream_message(&self, message: &Message) {
///         let mut file = self.file.lock().unwrap();
///         writeln!(file, "Stream complete: {}", serde_json::to_string(message).unwrap()).unwrap();
///     }
/// }
/// ```
pub trait ClientLogger: Send + Sync {
    /// Log a complete response from a non-streaming `send` call.
    ///
    /// This method is called once per successful `send` call with the full
    /// [`Message`] response from the API.
    fn log_response(&self, message: &Message);

    /// Log an individual streaming event.
    ///
    /// This method is called for each [`MessageStreamEvent`] received during
    /// a streaming request. Events include message starts, content deltas,
    /// and message stops.
    fn log_stream_event(&self, event: &MessageStreamEvent);

    /// Log the reconstructed message from a completed stream.
    ///
    /// This method is called once when a stream completes successfully,
    /// with the full [`Message`] that was reconstructed from all the
    /// streaming events.
    fn log_stream_message(&self, message: &Message);
}
