//! Avatar configuration types.
//!
//! Contains [`AvatarConfig`], [`LipSyncConfig`], [`RenderingConfig`], and
//! [`AvatarProviderKind`] for configuring video avatars in realtime sessions.

use serde::{Deserialize, Serialize};

/// Configuration for a video avatar in a realtime session.
///
/// Specifies the avatar source (image or video URL), optional lip-sync
/// settings, optional rendering parameters, and an optional provider selection.
///
/// # Backward Compatibility
///
/// The `provider` field is optional and defaults to `None`. JSON produced by
/// the previous version of `AvatarConfig` (without a `provider` field)
/// deserializes successfully with `provider: None`.
///
/// # Example
///
/// ```rust
/// use adk_realtime::avatar::{AvatarConfig, AvatarProviderKind};
///
/// let config = AvatarConfig {
///     source_url: "https://example.com/avatar.png".to_string(),
///     lip_sync: None,
///     rendering: None,
///     provider: Some(AvatarProviderKind::HeyGen),
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvatarConfig {
    /// URL to image or video file for the avatar source.
    pub source_url: String,
    /// Optional lip-sync configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lip_sync: Option<LipSyncConfig>,
    /// Optional rendering parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rendering: Option<RenderingConfig>,
    /// Avatar provider to use. When `None`, the system logs a warning
    /// and proceeds audio-only (backward-compatible behavior).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<AvatarProviderKind>,
}

/// Lip-sync configuration for a video avatar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LipSyncConfig {
    /// Whether lip-sync is enabled.
    pub enabled: bool,
    /// Sync mode (e.g., "viseme").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_mode: Option<String>,
}

/// Rendering parameters for a video avatar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderingConfig {
    /// Target resolution (e.g., "720p", "1080p").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    /// Target frame rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_rate: Option<u32>,
}

/// Identifies which avatar provider to use.
///
/// Used in [`AvatarConfig::provider`] to select the backend service
/// for video avatar rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AvatarProviderKind {
    /// HeyGen LiveAvatar (uses LiveKit for WebRTC transport).
    HeyGen,
    /// D-ID Realtime Agents (uses native WebRTC SDP/ICE).
    DId,
}
