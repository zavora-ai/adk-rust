//! OpenAI Realtime API provider.
//!
//! This module provides the OpenAI implementation of the realtime traits,
//! connecting to OpenAI's WebSocket-based Realtime API.
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_realtime::openai::OpenAIRealtimeModel;
//! use adk_realtime::{RealtimeModel, RealtimeConfig};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let model = OpenAIRealtimeModel::new(
//!         std::env::var("OPENAI_API_KEY")?,
//!         "gpt-4o-realtime-preview-2024-12-17",
//!     );
//!
//!     let config = RealtimeConfig::default()
//!         .with_instruction("You are a helpful assistant.")
//!         .with_voice("alloy");
//!
//!     let session = model.connect(config).await?;
//!
//!     // Use the session...
//!     session.close().await?;
//!     Ok(())
//! }
//! ```

mod model;
mod session;
#[cfg(feature = "openai-webrtc")]
pub mod webrtc;

pub use model::OpenAIRealtimeModel;
pub use session::OpenAIRealtimeSession;

#[cfg(feature = "openai-webrtc")]
pub use webrtc::OpenAIWebRTCSession;
#[cfg(feature = "openai-webrtc")]
pub use webrtc::OpusCodec;

/// OpenAI Realtime API WebSocket URL.
pub const OPENAI_REALTIME_URL: &str = "wss://api.openai.com/v1/realtime";

/// Available voices for OpenAI Realtime.
pub const OPENAI_VOICES: &[&str] =
    &["alloy", "ash", "ballad", "coral", "echo", "sage", "shimmer", "verse"];

/// Default model for OpenAI Realtime.
pub const DEFAULT_MODEL: &str = "gpt-4o-realtime-preview-2024-12-17";

/// Transport type for OpenAI Realtime connections.
///
/// By default, connections use WebSocket. When the `openai-webrtc` feature is
/// enabled, WebRTC transport is also available for lower-latency audio.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OpenAITransport {
    /// WebSocket transport (default).
    #[default]
    WebSocket,
    /// WebRTC transport for lower-latency audio.
    #[cfg(feature = "openai-webrtc")]
    WebRTC,
}
