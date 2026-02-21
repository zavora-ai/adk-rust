//! LiveKit WebRTC bridge for `adk-realtime`.
//!
//! This module provides a provider-agnostic bridge between LiveKit rooms and
//! realtime AI sessions. It re-exports the subset of [`livekit`] and
//! [`livekit_api`] types that are needed to build a voice agent, so downstream
//! crates only need `adk-realtime` in their `Cargo.toml`.
//!
//! # Provided utilities
//!
//! | Type | Purpose |
//! |------|---------|
//! | [`LiveKitEventHandler`] | Wraps any [`EventHandler`](crate::runner::EventHandler) to push model audio to a [`NativeAudioSource`]. |
//! | [`bridge_input`] | Reads audio from a [`RemoteAudioTrack`] and feeds 24 kHz PCM16 to a [`RealtimeRunner`](crate::RealtimeRunner). |
//! | [`bridge_gemini_input`] | Same as [`bridge_input`] but resamples to 16 kHz mono for Gemini Live. |
//!
//! # Feature flag
//!
//! This module requires the **`livekit`** Cargo feature:
//!
//! ```toml
//! [dependencies]
//! adk-realtime = { version = "0.3", features = ["livekit"] }
//! ```
//!
//! # Example
//!
//! ```rust,ignore
//! use adk_realtime::livekit::{
//!     AccessToken, AudioSourceOptions, LiveKitEventHandler, NativeAudioSource,
//!     Room, RoomOptions, RtcAudioSource, TrackPublishOptions, VideoGrants,
//!     bridge_gemini_input,
//! };
//!
//! // 1. Generate a token
//! let token = AccessToken::with_api_key(&key, &secret)
//!     .with_identity("agent")
//!     .with_grants(VideoGrants {
//!         room_join: true,
//!         room: "my-room".into(),
//!         ..Default::default()
//!     })
//!     .to_jwt()?;
//!
//! // 2. Connect to the room
//! let (room, mut events) = Room::connect(&url, &token, RoomOptions::default()).await?;
//!
//! // 3. Publish an audio track
//! let audio_source = NativeAudioSource::new(AudioSourceOptions::default(), 24000, 1);
//! let handler = LiveKitEventHandler::new(inner_handler, audio_source.clone());
//! ```

mod bridge;
mod handler;

// ── Our bridge utilities ────────────────────────────────────────────────

pub use bridge::{bridge_gemini_input, bridge_input};
pub use handler::LiveKitEventHandler;

// ── Room and connection ─────────────────────────────────────────────────
//
// Core types for connecting to and interacting with a LiveKit room.

pub use livekit::prelude::{
    ConnectionState, DataPacket, DataPacketKind, Room, RoomError, RoomEvent, RoomOptions,
    RoomResult,
};

// ── Participants ────────────────────────────────────────────────────────

pub use livekit::prelude::{LocalParticipant, Participant, RemoteParticipant};

// ── Tracks ──────────────────────────────────────────────────────────────
//
// Track types used when subscribing to remote audio or publishing local
// audio back into the room.

pub use livekit::prelude::{
    LocalAudioTrack, LocalTrack, RemoteAudioTrack, RemoteTrack, RemoteVideoTrack, Track, TrackKind,
    TrackSource,
};

/// Options for publishing a local track (codec preferences, simulcast, etc.).
pub use livekit::options::TrackPublishOptions;

// ── Audio I/O ───────────────────────────────────────────────────────────
//
// Low-level audio primitives for pushing PCM frames into a room or
// reading them from a subscribed track.

/// A single audio frame (PCM samples + sample rate + channel count).
pub use livekit::webrtc::audio_frame::AudioFrame;

/// Builder options, source wrapper, and platform-native source for
/// pushing audio frames into a LiveKit audio track.
pub use livekit::webrtc::audio_source::{
    AudioSourceOptions, RtcAudioSource, native::NativeAudioSource,
};

// ── Authentication ──────────────────────────────────────────────────────
//
// Token generation for room access. These come from the `livekit-api`
// crate so downstream consumers do not need a direct dependency on it.

/// JWT access token for authenticating participants.
pub use livekit_api::access_token::AccessToken;

/// Permission grants embedded in an [`AccessToken`].
pub use livekit_api::access_token::VideoGrants;

/// Convenience prelude that re-exports everything above plus our bridge
/// utilities.
///
/// ```rust,ignore
/// use adk_realtime::livekit::prelude::*;
/// ```
pub mod prelude {
    pub use super::{
        // Authentication
        AccessToken,
        // Audio I/O
        AudioFrame,
        AudioSourceOptions,
        // Room & connection
        ConnectionState,
        DataPacket,
        DataPacketKind,
        // Bridge utilities
        LiveKitEventHandler,
        // Tracks
        LocalAudioTrack,
        // Participants
        LocalParticipant,
        LocalTrack,
        NativeAudioSource,
        Participant,
        RemoteAudioTrack,
        RemoteParticipant,
        RemoteTrack,
        RemoteVideoTrack,
        Room,
        RoomError,
        RoomEvent,
        RoomOptions,
        RoomResult,
        RtcAudioSource,
        Track,
        TrackKind,
        TrackPublishOptions,
        TrackSource,
        VideoGrants,
        bridge_gemini_input,
        bridge_input,
    };
}
