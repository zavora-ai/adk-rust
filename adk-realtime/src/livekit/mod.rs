//! LiveKit WebRTC bridge for `adk-realtime`.
//!
//! This module provides a provider-agnostic bridge between LiveKit rooms and
//! realtime AI sessions. It includes:
//!
//! - [`LiveKitEventHandler`] — wraps any [`EventHandler`](crate::runner::EventHandler)
//!   to push model audio to a LiveKit [`NativeAudioSource`](livekit::native::NativeAudioSource).
//! - [`bridge_input`] — reads audio from a LiveKit [`RemoteAudioTrack`](livekit::track::RemoteAudioTrack)
//!   and feeds it to a [`RealtimeRunner`](crate::RealtimeRunner) as base64-encoded PCM16 at 24kHz.
//! - [`bridge_gemini_input`] — same as `bridge_input` but resamples to 16kHz mono for Gemini.
//!
//! # Feature Flag
//!
//! This module is only available when the `livekit` feature is enabled:
//!
//! ```toml
//! [dependencies]
//! adk-realtime = { version = "...", features = ["livekit"] }
//! ```

mod bridge;
mod handler;

pub use bridge::{bridge_gemini_input, bridge_input};
pub use handler::LiveKitEventHandler;

// Re-export core LiveKit types so downstream crates only need `adk-realtime`
pub use livekit::options::TrackPublishOptions;
pub use livekit::prelude::*;
pub use livekit::webrtc::audio_source::{
    AudioSourceOptions, RtcAudioSource, native::NativeAudioSource,
};
pub use livekit_api::access_token::{AccessToken, VideoGrants};
