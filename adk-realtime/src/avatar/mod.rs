//! Video avatar configuration and provider trait for realtime sessions.
//!
//! This module provides the [`AvatarProvider`] trait for pluggable video avatar
//! backends, along with configuration types ([`AvatarConfig`]) and session
//! metadata types ([`AvatarSessionInfo`], [`VideoStreamInfo`]).
//!
//! ## Feature Flags
//!
//! This module is gated behind the `video-avatar` feature flag:
//!
//! ```toml
//! [dependencies]
//! adk-realtime = { version = "...", features = ["video-avatar"] }
//! ```
//!
//! Concrete provider implementations are gated behind additional features:
//!
//! - `heygen-avatar` — HeyGen LiveAvatar (implies `video-avatar` + `livekit`)
//! - `did-avatar` — D-ID Realtime Agents (implies `video-avatar`)

pub mod config;
pub mod types;

pub use config::{AvatarConfig, AvatarProviderKind, LipSyncConfig, RenderingConfig};
pub use types::{AvatarSessionInfo, IceServer, VideoStreamInfo};

use std::sync::Arc;

use async_trait::async_trait;

use crate::error::RealtimeError;

/// Result type alias for avatar provider operations.
pub type AvatarResult<T> = Result<T, RealtimeError>;

/// A pluggable video avatar backend.
///
/// Implementations manage the full lifecycle of an avatar session:
/// creating the session with the external provider, routing audio
/// frames for lip-sync rendering, and tearing down the session.
///
/// # Object Safety
///
/// This trait is object-safe and can be used as `Arc<dyn AvatarProvider>`.
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use adk_realtime::avatar::{AvatarProvider, AvatarConfig, AvatarSessionInfo, AvatarResult};
///
/// let provider: Arc<dyn AvatarProvider> = create_provider();
/// let config = AvatarConfig { /* ... */ };
///
/// let session = provider.start_session(&config).await?;
/// provider.send_audio(&session.session_id, &audio_bytes).await?;
/// provider.stop_session(&session.session_id).await?;
/// ```
#[async_trait]
pub trait AvatarProvider: Send + Sync + std::fmt::Debug {
    /// Human-readable provider name (e.g., "heygen", "d-id").
    fn name(&self) -> &str;

    /// Start an avatar session.
    ///
    /// Creates a new session with the external avatar service using the
    /// given configuration. Returns session metadata including how the
    /// client should connect to receive the video stream.
    ///
    /// # Errors
    ///
    /// Returns `RealtimeError` if the provider API call fails or the
    /// transport connection cannot be established.
    async fn start_session(&self, config: &AvatarConfig) -> AvatarResult<AvatarSessionInfo>;

    /// Send audio for lip-sync rendering.
    ///
    /// The audio data should be PCM16 mono at the sample rate expected
    /// by the provider (typically 24kHz for HeyGen, 16kHz for D-ID).
    ///
    /// # Errors
    ///
    /// Returns `RealtimeError` if the session is not active or audio
    /// delivery fails.
    async fn send_audio(&self, session_id: &str, audio: &[u8]) -> AvatarResult<()>;

    /// Send keep-alive to prevent idle timeout.
    ///
    /// Should be called periodically while the session is active to
    /// prevent the provider from closing the session due to inactivity.
    ///
    /// # Errors
    ///
    /// Returns `RealtimeError` if the session is not active.
    async fn keep_alive(&self, session_id: &str) -> AvatarResult<()>;

    /// Stop the session and release resources.
    ///
    /// Terminates the provider session and releases all transport
    /// resources (LiveKit rooms, WebRTC peers, etc.). Calling this
    /// on an already-stopped session is a no-op returning `Ok(())`.
    ///
    /// # Errors
    ///
    /// Returns `RealtimeError` if the provider API call to stop the
    /// session fails.
    async fn stop_session(&self, session_id: &str) -> AvatarResult<()>;

    /// Check if a session is still active.
    async fn is_active(&self, session_id: &str) -> bool;
}

// Ensure the trait is object-safe by verifying it can be used as a trait object.
const _: () = {
    fn _assert_object_safe(_: &dyn AvatarProvider) {}
    fn _assert_arc_compatible(_: Arc<dyn AvatarProvider>) {}
};
