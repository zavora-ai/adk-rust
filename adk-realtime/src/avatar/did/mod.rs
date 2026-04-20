//! D-ID Realtime Agents provider for `adk-realtime`.
//!
//! This module implements the [`AvatarProvider`] trait for D-ID's agent
//! session API. D-ID uses standard WebRTC SDP/ICE for audio/video transport.
//! The provider creates an agent chat session via D-ID's REST API, returns
//! the SDP offer and ICE servers so the **client** can establish the WebRTC
//! connection directly with D-ID's peer.
//!
//! ## Architecture Note
//!
//! D-ID agents handle their own LLM and TTS internally. The ADK agent's
//! role is to configure the D-ID agent (LLM, knowledge base) and manage
//! the session lifecycle. Audio/video flows directly between the client
//! and D-ID's WebRTC peer — the server-side provider does not relay audio.
//! Consequently, [`AvatarProvider::send_audio()`] is a no-op for D-ID.
//!
//! # Feature Flag
//!
//! This module requires the **`did-avatar`** Cargo feature:
//!
//! ```toml
//! [dependencies]
//! adk-realtime = { version = "...", features = ["did-avatar"] }
//! ```
//!
//! # Example
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use adk_realtime::avatar::did::{DIDConfig, DIDProvider};
//! use adk_realtime::avatar::{AvatarConfig, AvatarProvider, AvatarProviderKind};
//!
//! let provider = DIDProvider::new(
//!     DIDConfig::new("your-api-key", "your-agent-id"),
//! );
//!
//! let config = AvatarConfig {
//!     source_url: "https://example.com/avatar.jpg".to_string(),
//!     lip_sync: None,
//!     rendering: None,
//!     provider: Some(AvatarProviderKind::DId),
//! };
//!
//! let session = provider.start_session(&config).await?;
//! // Client uses session.video_stream (WebRTC SDP/ICE) to connect directly.
//! provider.stop_session(&session.session_id).await?;
//! ```

pub mod api;
pub mod config;

pub use config::{DIDConfig, DIDLlmConfig};

use std::collections::HashMap;

use async_trait::async_trait;
use secrecy::ExposeSecret;
use tokio::sync::RwLock;

use super::types::{AvatarSessionInfo, VideoStreamInfo};
use super::{AvatarProvider, AvatarResult};
use crate::error::RealtimeError;

/// Internal state for an active D-ID session.
#[derive(Debug)]
struct DIDSession {
    /// The D-ID chat ID (returned as `id` in the create response).
    #[allow(dead_code)]
    chat_id: String,
}

/// D-ID Realtime Agents provider.
///
/// Uses D-ID's REST API for agent session management. The WebRTC
/// connection is established directly between the client and D-ID's
/// peer — this provider handles only the REST lifecycle (create,
/// signal SDP, delete).
///
/// Since D-ID agents handle their own LLM and TTS internally,
/// [`send_audio()`](AvatarProvider::send_audio) is a no-op. Audio
/// flows from D-ID's TTS directly to the client via WebRTC.
pub struct DIDProvider {
    config: DIDConfig,
    http_client: reqwest::Client,
    sessions: RwLock<HashMap<String, DIDSession>>,
}

impl std::fmt::Debug for DIDProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DIDProvider")
            .field("config", &self.config)
            .field("sessions_count", &"<locked>")
            .finish()
    }
}

impl DIDProvider {
    /// Create a new `DIDProvider` with the given configuration.
    ///
    /// # Panics
    ///
    /// Panics if `api_base_url` does not use HTTPS (cleartext transport
    /// would expose API keys and session data).
    pub fn new(config: DIDConfig) -> Self {
        assert!(
            config.api_base_url.starts_with("https://"),
            "d-id: api_base_url must use https:// for secure transport, got: {}",
            config.api_base_url
        );
        Self { config, http_client: reqwest::Client::new(), sessions: RwLock::new(HashMap::new()) }
    }

    /// Build a URL under the API base, enforcing HTTPS.
    fn secure_url(&self, path: &str) -> AvatarResult<String> {
        if !self.config.api_base_url.starts_with("https://") {
            return Err(RealtimeError::provider(
                "d-id: api_base_url must use https:// for secure transport",
            ));
        }
        Ok(format!("{}{path}", self.config.api_base_url))
    }
}

#[async_trait]
impl AvatarProvider for DIDProvider {
    fn name(&self) -> &str {
        "d-id"
    }

    async fn start_session(
        &self,
        avatar_config: &super::config::AvatarConfig,
    ) -> AvatarResult<AvatarSessionInfo> {
        // Validate inputs.
        if self.config.agent_id.is_empty() {
            return Err(RealtimeError::config("d-id: agent_id must not be empty"));
        }
        if avatar_config.source_url.is_empty() {
            return Err(RealtimeError::config("d-id: avatar source_url must not be empty"));
        }

        // Step 1: Build the create-session request.
        let request_body = api::CreateSessionRequest {
            source_url: avatar_config.source_url.clone(),
            llm: self.config.llm_config.clone(),
            knowledge_id: self.config.knowledge_id.clone(),
        };

        let url = self.secure_url(&format!("/agents/{}/chat", self.config.agent_id))?;
        tracing::info!(url = %url, "d-id: creating agent chat session");

        // Step 2: Call D-ID REST API.
        let response = self
            .http_client
            .post(&url)
            .header("Authorization", format!("Basic {}", self.config.api_key.expose_secret()))
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await
            .map_err(|e| RealtimeError::provider(format!("d-id: REST request failed: {e}")))?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(RealtimeError::AuthError(format!(
                "d-id: authentication failed (HTTP {status})"
            )));
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(RealtimeError::provider(format!(
                "d-id: session creation failed (HTTP {status}): {body}"
            )));
        }

        // Step 3: Parse the response containing SDP offer and ICE servers.
        let session_response: api::CreateSessionResponse = response
            .json()
            .await
            .map_err(|e| RealtimeError::provider(format!("d-id: failed to parse response: {e}")))?;

        let session_id = session_response.session_id.clone();
        let chat_id = session_response.id.clone();

        tracing::info!(
            session_id = %session_id,
            chat_id = %chat_id,
            ice_servers = session_response.ice_servers.len(),
            "d-id: session created with SDP offer"
        );

        // Step 4: Build session info for the client.
        // The client is responsible for creating the WebRTC peer connection,
        // setting the remote description (SDP offer), creating an SDP answer,
        // and sending it back to D-ID. We return the offer and ICE servers
        // so the client can do this.
        let session_info = AvatarSessionInfo {
            session_id: session_id.clone(),
            video_stream: VideoStreamInfo::WebRTC {
                sdp_answer: session_response.offer,
                ice_servers: session_response.ice_servers,
            },
            provider: "d-id".to_string(),
        };

        // Step 5: Store session state.
        let did_session = DIDSession { chat_id };
        self.sessions.write().await.insert(session_id, did_session);

        Ok(session_info)
    }

    async fn send_audio(&self, session_id: &str, _audio: &[u8]) -> AvatarResult<()> {
        // D-ID agents handle their own LLM and TTS internally. Audio flows
        // directly from D-ID's WebRTC peer to the client. The server-side
        // provider does not relay audio — this is a no-op.
        let sessions = self.sessions.read().await;
        if !sessions.contains_key(session_id) {
            return Err(RealtimeError::provider(format!(
                "d-id: no active session with id '{session_id}'"
            )));
        }

        tracing::debug!(
            session_id = %session_id,
            "d-id: send_audio is a no-op (D-ID renders from its own TTS)"
        );
        Ok(())
    }

    async fn keep_alive(&self, session_id: &str) -> AvatarResult<()> {
        // D-ID manages its own session timeout via the WebRTC connection.
        // No explicit keep-alive endpoint is needed.
        let sessions = self.sessions.read().await;
        if !sessions.contains_key(session_id) {
            return Err(RealtimeError::provider(format!(
                "d-id: no active session with id '{session_id}'"
            )));
        }

        tracing::debug!(
            session_id = %session_id,
            "d-id: keep_alive is a no-op (D-ID manages session timeout internally)"
        );
        Ok(())
    }

    async fn stop_session(&self, session_id: &str) -> AvatarResult<()> {
        // Remove the session from our map. If it doesn't exist, it's a no-op.
        let session = self.sessions.write().await.remove(session_id);
        let Some(_session) = session else {
            tracing::debug!(session_id = %session_id, "d-id: session already stopped (no-op)");
            return Ok(());
        };

        // Call D-ID REST API to delete the agent chat session.
        // Enforce HTTPS to prevent cleartext transmission of session identifiers.
        let url =
            self.secure_url(&format!("/agents/{}/chat/{session_id}", self.config.agent_id))?;
        tracing::info!(session_id = %session_id, "d-id: stopping session");

        let result = self
            .http_client
            .delete(&url)
            .header("Authorization", format!("Basic {}", self.config.api_key.expose_secret()))
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
                    "d-id: stop session API returned non-success status"
                );
            }
            Err(e) => {
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    "d-id: stop session API request failed"
                );
            }
            Ok(_) => {
                tracing::info!(session_id = %session_id, "d-id: session stopped via API");
            }
        }

        Ok(())
    }

    async fn is_active(&self, session_id: &str) -> bool {
        self.sessions.read().await.contains_key(session_id)
    }
}
