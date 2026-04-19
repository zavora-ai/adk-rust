//! Session and stream types for avatar providers.
//!
//! Contains [`AvatarSessionInfo`], [`VideoStreamInfo`], and [`IceServer`]
//! types used to communicate avatar session state between the provider,
//! the runner, and the client.

use serde::{Deserialize, Serialize};

/// Metadata about an active avatar session.
///
/// Returned by [`super::AvatarProvider::start_session()`] after successfully
/// creating a session with the external avatar service.
///
/// # Example
///
/// ```rust
/// use adk_realtime::avatar::{AvatarSessionInfo, VideoStreamInfo};
///
/// let info = AvatarSessionInfo {
///     session_id: "sess_abc123".to_string(),
///     video_stream: VideoStreamInfo::StreamUrl {
///         url: "https://stream.example.com/live".to_string(),
///     },
///     provider: "heygen".to_string(),
/// };
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvatarSessionInfo {
    /// Provider-assigned session identifier.
    pub session_id: String,
    /// How the client connects to the avatar video stream.
    pub video_stream: VideoStreamInfo,
    /// Provider name (e.g., "heygen", "d-id").
    pub provider: String,
}

/// Describes how the client connects to the avatar video stream.
///
/// Each variant corresponds to a different transport mechanism used
/// by avatar providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum VideoStreamInfo {
    /// Client joins a LiveKit room to receive video.
    LiveKit {
        /// LiveKit server URL.
        url: String,
        /// Authentication token for the LiveKit room.
        token: String,
        /// Name of the LiveKit room.
        room_name: String,
    },
    /// Client uses WebRTC SDP/ICE to receive video.
    WebRTC {
        /// SDP answer for the WebRTC connection.
        sdp_answer: String,
        /// ICE server configurations for NAT traversal.
        ice_servers: Vec<IceServer>,
    },
    /// Client fetches video from a URL (HLS, DASH, etc.).
    StreamUrl {
        /// URL of the video stream.
        url: String,
    },
}

/// ICE server configuration for WebRTC connections.
///
/// Used in [`VideoStreamInfo::WebRTC`] to provide STUN/TURN server
/// details for NAT traversal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IceServer {
    /// STUN or TURN server URLs.
    pub urls: Vec<String>,
    /// Optional username for TURN authentication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Optional credential for TURN authentication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}
