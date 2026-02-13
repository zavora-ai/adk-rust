//! Gemini Live API provider.
//!
//! This module provides the Gemini implementation of the realtime traits,
//! connecting to Google's WebSocket-based Live API.
//!
//! # Status
//!
//! This provider is currently a work in progress. The Gemini Live API has
//! some differences from OpenAI's Realtime API:
//!
//! - Input audio: 16kHz mono PCM
//! - Output audio: 24kHz mono PCM
//! - Different VAD settings
//! - Different tool calling format
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_realtime::gemini::GeminiRealtimeModel;
//! use adk_realtime::{RealtimeModel, RealtimeConfig};
//! use adk_gemini::GeminiLiveBackend;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let backend = GeminiLiveBackend::Studio {
//!         api_key: std::env::var("GOOGLE_API_KEY")?
//!     };
//!     
//!     let model = GeminiRealtimeModel::new(
//!         backend,
//!         "models/gemini-live-2.5-flash-native-audio",
//!     );
//!
//!     let config = RealtimeConfig::default()
//!         .with_instruction("You are a helpful assistant.");
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

pub use model::GeminiRealtimeModel;
pub use session::GeminiRealtimeSession;

/// Gemini Live API WebSocket URL template.
pub const GEMINI_LIVE_URL: &str = "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent";

/// Default model for Gemini Live.
pub const DEFAULT_MODEL: &str = "models/gemini-live-2.5-flash-native-audio";

/// Available voices for Gemini Live (varies by model).
pub const GEMINI_VOICES: &[&str] = &["Puck", "Charon", "Kore", "Fenrir", "Aoede"];
