//! HeyGen LiveAvatar provider for `adk-realtime`.
//!
//! This module implements the [`AvatarProvider`] trait for HeyGen's streaming
//! avatar API. HeyGen uses LiveKit as the WebRTC transport layer — the provider
//! creates a streaming session via HeyGen's REST API, connects to the returned
//! LiveKit room, and publishes agent audio to the room's audio track.
//!
//! # Feature Flag
//!
//! This module requires the **`heygen-avatar`** Cargo feature:
//!
//! ```toml
//! [dependencies]
//! adk-realtime = { version = "...", features = ["heygen-avatar"] }
//! ```
//!
//! # Example
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use adk_realtime::avatar::heygen::{HeyGenConfig, HeyGenProvider, HeyGenQuality};
//! use adk_realtime::avatar::{AvatarConfig, AvatarProvider, AvatarProviderKind};
//!
//! let provider = HeyGenProvider::new(
//!     HeyGenConfig::new("your-api-key")
//!         .with_quality(HeyGenQuality::High)
//!         .with_idle_timeout(300),
//! );
//!
//! let config = AvatarConfig {
//!     source_url: "avatar_id_from_heygen".to_string(),
//!     lip_sync: None,
//!     rendering: None,
//!     provider: Some(AvatarProviderKind::HeyGen),
//! };
//!
//! let session = provider.start_session(&config).await?;
//! provider.send_audio(&session.session_id, &pcm16_audio).await?;
//! provider.stop_session(&session.session_id).await?;
//! ```

pub mod api;
pub mod config;

pub use config::{HeyGenConfig, HeyGenQuality};

use std::borrow::Cow;
use std::collections::HashMap;

use async_trait::async_trait;
use livekit::webrtc::audio_frame::AudioFrame;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_source::{AudioSourceOptions, RtcAudioSource};
use secrecy::ExposeSecret;
use tokio::sync::RwLock;

use super::types::{AvatarSessionInfo, VideoStreamInfo};
use super::{AvatarProvider, AvatarResult};
use crate::error::RealtimeError;
use crate::livekit::{LocalAudioTrack, LocalTrack, Room, RoomOptions, TrackPublishOptions};

/// Internal state for an active HeyGen session.
#[allow(dead_code)]
struct HeyGenSession {
    /// LiveKit room connection.
    room: Room,
    /// Audio source for publishing PCM16 frames to the LiveKit room.
    audio_source: NativeAudioSource,
    /// LiveKit server URL (stored for session info).
    livekit_url: String,
    /// LiveKit access token (stored for session info).
    access_token: String,
}

impl std::fmt::Debug for HeyGenSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeyGenSession")
            .field("livekit_url", &self.livekit_url)
            .field("access_token", &"[REDACTED]")
            .finish()
    }
}

/// HeyGen LiveAvatar provider.
///
/// Uses HeyGen's REST API for session management and LiveKit for
/// WebRTC audio/video transport. Each session creates a LiveKit room
/// where the provider publishes agent audio and the client receives
/// lip-synced video.
pub struct HeyGenProvider {
    config: HeyGenConfig,
    http_client: reqwest::Client,
    sessions: RwLock<HashMap<String, HeyGenSession>>,
}

impl std::fmt::Debug for HeyGenProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeyGenProvider")
            .field("config", &self.config)
            .field("sessions_count", &"<locked>")
            .finish()
    }
}

impl HeyGenProvider {
    /// Create a new `HeyGenProvider` with the given configuration.
    ///
    /// # Panics
    ///
    /// Panics if `api_base_url` does not use HTTPS (cleartext transport
    /// would expose API keys and session data).
    pub fn new(config: HeyGenConfig) -> Self {
        assert!(
            config.api_base_url.starts_with("https://"),
            "heygen: api_base_url must use https:// for secure transport, got: {}",
            config.api_base_url
        );
        Self { config, http_client: reqwest::Client::new(), sessions: RwLock::new(HashMap::new()) }
    }

    /// Build a URL under the API base, enforcing HTTPS.
    fn secure_url(&self, path: &str) -> AvatarResult<String> {
        if !self.config.api_base_url.starts_with("https://") {
            return Err(RealtimeError::provider(
                "heygen: api_base_url must use https:// for secure transport",
            ));
        }
        Ok(format!("{}{path}", self.config.api_base_url))
    }
}

/// HeyGen expects 24 kHz mono audio.
const HEYGEN_SAMPLE_RATE: u32 = 24000;
/// Mono audio channel.
const HEYGEN_NUM_CHANNELS: u32 = 1;

#[async_trait]
impl AvatarProvider for HeyGenProvider {
    fn name(&self) -> &str {
        "heygen"
    }

    async fn start_session(
        &self,
        avatar_config: &super::config::AvatarConfig,
    ) -> AvatarResult<AvatarSessionInfo> {
        // Step 1: Use source_url as the avatar_id for HeyGen.
        let avatar_id = avatar_config.source_url.clone();
        if avatar_id.is_empty() {
            return Err(RealtimeError::config(
                "heygen: avatar source_url (used as avatar_id) must not be empty",
            ));
        }

        // Step 2: Call HeyGen REST API to create a streaming session.
        let request_body = api::CreateSessionRequest {
            avatar_id,
            quality: self.config.quality,
            version: Some("v2".to_string()),
        };

        let url = self.secure_url("/v1/streaming.new")?;
        tracing::info!(url = %url, "heygen: creating streaming session");

        let response = self
            .http_client
            .post(&url)
            .header("x-api-key", self.config.api_key.expose_secret())
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await
            .map_err(|e| RealtimeError::provider(format!("heygen: REST request failed: {e}")))?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(RealtimeError::AuthError(format!(
                "heygen: authentication failed (HTTP {status})"
            )));
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(RealtimeError::provider(format!(
                "heygen: session creation failed (HTTP {status}): {body}"
            )));
        }

        let session_response: api::CreateSessionResponse = response.json().await.map_err(|e| {
            RealtimeError::provider(format!("heygen: failed to parse response: {e}"))
        })?;

        let data = session_response.data;
        let session_id = data.session_id.clone();
        let livekit_url = data.url.clone();
        let access_token = data.access_token.clone();

        tracing::info!(session_id = %session_id, "heygen: session created, connecting to LiveKit room");

        // Step 3: Connect to the LiveKit room.
        let (room, _events) =
            Room::connect(&livekit_url, &access_token, RoomOptions::default()).await.map_err(
                |e| RealtimeError::provider(format!("heygen: LiveKit room connection failed: {e}")),
            )?;

        // Step 4: Set up audio source for publishing.
        let audio_source = NativeAudioSource::new(
            AudioSourceOptions::default(),
            HEYGEN_SAMPLE_RATE,
            HEYGEN_NUM_CHANNELS,
            HEYGEN_SAMPLE_RATE / 100,
        );

        // Step 5: Publish audio track to the room.
        let audio_track = LocalAudioTrack::create_audio_track(
            "agent-audio",
            RtcAudioSource::Native(audio_source.clone()),
        );
        room.local_participant()
            .publish_track(LocalTrack::Audio(audio_track), TrackPublishOptions::default())
            .await
            .map_err(|e| {
                RealtimeError::provider(format!("heygen: failed to publish audio track: {e}"))
            })?;

        tracing::info!(session_id = %session_id, "heygen: audio track published to LiveKit room");

        // Step 6: Build session info for the client.
        let room_name = room.name().to_string();
        let session_info = AvatarSessionInfo {
            session_id: session_id.clone(),
            video_stream: VideoStreamInfo::LiveKit {
                url: livekit_url.clone(),
                token: access_token.clone(),
                room_name,
            },
            provider: "heygen".to_string(),
        };

        // Step 7: Store session state.
        let heygen_session = HeyGenSession { room, audio_source, livekit_url, access_token };
        self.sessions.write().await.insert(session_id, heygen_session);

        Ok(session_info)
    }

    async fn send_audio(&self, session_id: &str, audio: &[u8]) -> AvatarResult<()> {
        let sessions = self.sessions.read().await;
        let session = sessions.get(session_id).ok_or_else(|| {
            RealtimeError::provider(format!("heygen: no active session with id '{session_id}'"))
        })?;

        // Convert raw bytes to i16 samples (PCM16 little-endian).
        let samples_cow: Cow<'_, [i16]> = {
            #[cfg(target_endian = "little")]
            if let Ok(aligned_slice) = bytemuck::try_cast_slice::<u8, i16>(audio) {
                Cow::Borrowed(aligned_slice)
            } else {
                let fallback: Vec<i16> = audio
                    .chunks_exact(2)
                    .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
                    .collect();
                Cow::Owned(fallback)
            }

            #[cfg(not(target_endian = "little"))]
            {
                let samples: Vec<i16> = audio
                    .chunks_exact(2)
                    .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
                    .collect();
                Cow::Owned(samples)
            }
        };

        if samples_cow.is_empty() {
            return Ok(());
        }

        let samples_per_channel = samples_cow.len() as u32 / HEYGEN_NUM_CHANNELS;
        let frame = AudioFrame {
            data: samples_cow,
            sample_rate: HEYGEN_SAMPLE_RATE,
            num_channels: HEYGEN_NUM_CHANNELS,
            samples_per_channel,
        };

        session.audio_source.capture_frame(&frame).await.map_err(|e| {
            RealtimeError::provider(format!("heygen: failed to push audio frame to LiveKit: {e}"))
        })?;

        Ok(())
    }

    async fn keep_alive(&self, session_id: &str) -> AvatarResult<()> {
        // Verify the session exists.
        {
            let sessions = self.sessions.read().await;
            if !sessions.contains_key(session_id) {
                return Err(RealtimeError::provider(format!(
                    "heygen: no active session with id '{session_id}'"
                )));
            }
        }

        // Send a keep-alive task request to HeyGen.
        let request_body =
            api::TaskRequest { session_id: session_id.to_string(), text: String::new() };

        let url = self.secure_url("/v1/streaming.task")?;
        let response = self
            .http_client
            .post(&url)
            .header("x-api-key", self.config.api_key.expose_secret())
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await
            .map_err(|e| {
                RealtimeError::provider(format!("heygen: keep-alive request failed: {e}"))
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(RealtimeError::provider(format!(
                "heygen: keep-alive failed (HTTP {status}): {body}"
            )));
        }

        tracing::debug!(session_id = %session_id, "heygen: keep-alive sent");
        Ok(())
    }

    async fn stop_session(&self, session_id: &str) -> AvatarResult<()> {
        // Remove the session from our map. If it doesn't exist, it's a no-op.
        let session = self.sessions.write().await.remove(session_id);
        let Some(session) = session else {
            tracing::debug!(session_id = %session_id, "heygen: session already stopped (no-op)");
            return Ok(());
        };

        // Step 1: Call HeyGen REST API to stop the streaming session.
        let request_body = api::StopSessionRequest { session_id: session_id.to_string() };

        let url = self.secure_url("/v1/streaming.stop")?;
        tracing::info!(session_id = %session_id, "heygen: stopping session");

        let result = self
            .http_client
            .post(&url)
            .header("x-api-key", self.config.api_key.expose_secret())
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await;

        match result {
            Ok(response) if !response.status().is_success() => {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                tracing::warn!(
                    session_id = %session_id,
                    status = %status,
                    body = %body,
                    "heygen: stop session API returned non-success status"
                );
            }
            Err(e) => {
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    "heygen: stop session API request failed"
                );
            }
            Ok(_) => {
                tracing::info!(session_id = %session_id, "heygen: session stopped via API");
            }
        }

        // Step 2: Disconnect from the LiveKit room.
        let _ = session.room.close().await;
        tracing::debug!(session_id = %session_id, "heygen: LiveKit room disconnected");

        Ok(())
    }

    async fn is_active(&self, session_id: &str) -> bool {
        self.sessions.read().await.contains_key(session_id)
    }
}
