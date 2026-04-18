//! Video avatar configuration for realtime sessions.
//!
//! Provides [`AvatarConfig`] for attaching a video avatar to a realtime agent,
//! including lip-sync and rendering settings.
//!
//! ## Feature Flag
//!
//! This module is gated behind the `video-avatar` feature flag:
//!
//! ```toml
//! [dependencies]
//! adk-realtime = { version = "...", features = ["video-avatar"] }
//! ```

use serde::{Deserialize, Serialize};

/// Configuration for a video avatar in a realtime session.
///
/// Specifies the avatar source (image or video URL), optional lip-sync
/// settings, and optional rendering parameters.
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
