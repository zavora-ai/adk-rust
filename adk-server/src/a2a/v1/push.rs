//! A2A v1.0.0 push notification sender.
//!
//! Defines the [`PushNotificationSender`] trait for delivering task updates
//! to client-registered webhook endpoints. Includes [`HttpPushNotificationSender`]
//! with retry logic and SSRF validation, and [`NoOpPushNotificationSender`] for
//! development and testing.

use std::net::IpAddr;
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;

use a2a_protocol_types::TaskPushNotificationConfig;
use a2a_protocol_types::events::{TaskArtifactUpdateEvent, TaskStatusUpdateEvent};

use super::error::A2aError;

/// Maximum number of retry attempts for webhook delivery.
const MAX_RETRIES: u32 = 3;

/// Delay in seconds for each retry attempt (exponential backoff).
const RETRY_DELAYS: &[u64] = &[1, 2, 4];

/// Async trait for delivering push notifications to webhook endpoints.
///
/// Implementations must be `Send + Sync` for use across async boundaries.
#[async_trait]
pub trait PushNotificationSender: Send + Sync {
    /// Delivers a task status update to the configured webhook.
    async fn send_status_update(
        &self,
        url: &str,
        event: &TaskStatusUpdateEvent,
        config: &TaskPushNotificationConfig,
    ) -> Result<(), A2aError>;

    /// Delivers a task artifact update to the configured webhook.
    async fn send_artifact_update(
        &self,
        url: &str,
        event: &TaskArtifactUpdateEvent,
        config: &TaskPushNotificationConfig,
    ) -> Result<(), A2aError>;
}

/// No-op push notification sender for development and testing.
///
/// All delivery attempts succeed immediately without sending any HTTP requests.
pub struct NoOpPushNotificationSender;

#[async_trait]
impl PushNotificationSender for NoOpPushNotificationSender {
    async fn send_status_update(
        &self,
        _url: &str,
        _event: &TaskStatusUpdateEvent,
        _config: &TaskPushNotificationConfig,
    ) -> Result<(), A2aError> {
        Ok(())
    }

    async fn send_artifact_update(
        &self,
        _url: &str,
        _event: &TaskArtifactUpdateEvent,
        _config: &TaskPushNotificationConfig,
    ) -> Result<(), A2aError> {
        Ok(())
    }
}

/// HTTP-based push notification sender with retry and SSRF validation.
///
/// Uses `reqwest::Client` to POST JSON payloads to webhook URLs. Retries
/// up to 3 times with exponential backoff (1s, 2s, 4s) on failure. Validates
/// webhook URLs to reject private IP ranges and localhost (SSRF prevention).
pub struct HttpPushNotificationSender {
    client: reqwest::Client,
}

impl HttpPushNotificationSender {
    /// Creates a new sender with a default `reqwest::Client`.
    pub fn new() -> Self {
        Self { client: reqwest::Client::new() }
    }

    /// Creates a new sender with a custom `reqwest::Client`.
    pub fn with_client(client: reqwest::Client) -> Self {
        Self { client }
    }

    /// Sends a JSON payload to the given URL with retry logic.
    async fn send_with_retry(
        &self,
        url: &str,
        body: &impl Serialize,
        config: &TaskPushNotificationConfig,
    ) -> Result<(), A2aError> {
        validate_webhook_url(url)?;

        for attempt in 0..=MAX_RETRIES {
            let mut request = self.client.post(url).json(body);

            // Add Bearer auth if configured
            if let Some(ref auth) = config.authentication {
                request = request.header("Authorization", format!("Bearer {}", auth.credentials));
            }

            // Add notification token if configured
            if let Some(ref token) = config.token {
                request = request.header("a2a-notification-token", token);
            }

            match request.send().await {
                Ok(resp) if resp.status().is_success() => return Ok(()),
                Ok(resp) => {
                    tracing::warn!(
                        attempt,
                        status = %resp.status(),
                        url,
                        "push notification delivery received non-success status"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        attempt,
                        error = %e,
                        url,
                        "push notification delivery request failed"
                    );
                }
            }
            if attempt < MAX_RETRIES {
                tokio::time::sleep(Duration::from_secs(RETRY_DELAYS[attempt as usize])).await;
            }
        }

        tracing::error!(
            retries = MAX_RETRIES,
            url,
            "push notification delivery failed after all retries"
        );
        Err(A2aError::PushDeliveryFailed {
            message: format!("delivery failed after {MAX_RETRIES} retries"),
        })
    }
}

impl Default for HttpPushNotificationSender {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PushNotificationSender for HttpPushNotificationSender {
    async fn send_status_update(
        &self,
        url: &str,
        event: &TaskStatusUpdateEvent,
        config: &TaskPushNotificationConfig,
    ) -> Result<(), A2aError> {
        self.send_with_retry(url, event, config).await
    }

    async fn send_artifact_update(
        &self,
        url: &str,
        event: &TaskArtifactUpdateEvent,
        config: &TaskPushNotificationConfig,
    ) -> Result<(), A2aError> {
        self.send_with_retry(url, event, config).await
    }
}

/// Validates a webhook URL to prevent SSRF attacks.
///
/// Rejects URLs that resolve to:
/// - Private IP ranges: 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
/// - Loopback addresses: 127.0.0.0/8, ::1
/// - Localhost hostname
///
/// Returns `A2aError::InvalidParams` for invalid or rejected URLs.
pub fn validate_webhook_url(url: &str) -> Result<(), A2aError> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| A2aError::InvalidParams { message: format!("invalid webhook URL: {e}") })?;

    let host = parsed.host_str().ok_or_else(|| A2aError::InvalidParams {
        message: "webhook URL has no host".to_string(),
    })?;

    // Reject localhost hostname
    if host.eq_ignore_ascii_case("localhost") {
        return Err(A2aError::InvalidParams {
            message: "webhook URL must not target localhost".to_string(),
        });
    }

    // Check if host is an IP address
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_private_or_loopback(&ip) {
            return Err(A2aError::InvalidParams {
                message: format!("webhook URL must not target private/loopback address: {ip}"),
            });
        }
    }

    // Also check bracketed IPv6 (e.g., [::1])
    let trimmed = host.trim_start_matches('[').trim_end_matches(']');
    if trimmed != host {
        if let Ok(ip) = trimmed.parse::<IpAddr>() {
            if is_private_or_loopback(&ip) {
                return Err(A2aError::InvalidParams {
                    message: format!("webhook URL must not target private/loopback address: {ip}"),
                });
            }
        }
    }

    Ok(())
}

/// Returns `true` if the IP address is in a private range or is a loopback address.
fn is_private_or_loopback(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            // 127.0.0.0/8 (loopback)
            octets[0] == 127
            // 10.0.0.0/8
            || octets[0] == 10
            // 172.16.0.0/12
            || (octets[0] == 172 && (16..=31).contains(&octets[1]))
            // 192.168.0.0/16
            || (octets[0] == 192 && octets[1] == 168)
        }
        IpAddr::V6(v6) => {
            // ::1 (loopback)
            v6.is_loopback()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use a2a_protocol_types::{AuthenticationInfo, TaskPushNotificationConfig};

    #[test]
    fn test_validate_public_ip_accepted() {
        assert!(validate_webhook_url("https://8.8.8.8/webhook").is_ok());
        assert!(validate_webhook_url("https://1.2.3.4:8080/hook").is_ok());
        assert!(validate_webhook_url("https://203.0.113.1/callback").is_ok());
    }

    #[test]
    fn test_validate_public_domain_accepted() {
        assert!(validate_webhook_url("https://example.com/webhook").is_ok());
        assert!(validate_webhook_url("https://hooks.slack.com/services/abc").is_ok());
        assert!(validate_webhook_url("https://api.github.com/hooks").is_ok());
    }

    #[test]
    fn test_validate_private_10_rejected() {
        assert!(validate_webhook_url("https://10.0.0.1/webhook").is_err());
        assert!(validate_webhook_url("https://10.255.255.255/webhook").is_err());
        assert!(validate_webhook_url("https://10.1.2.3:8080/hook").is_err());
    }

    #[test]
    fn test_validate_private_172_rejected() {
        assert!(validate_webhook_url("https://172.16.0.1/webhook").is_err());
        assert!(validate_webhook_url("https://172.31.255.255/webhook").is_err());
        // 172.15.x.x is NOT private
        assert!(validate_webhook_url("https://172.15.0.1/webhook").is_ok());
        // 172.32.x.x is NOT private
        assert!(validate_webhook_url("https://172.32.0.1/webhook").is_ok());
    }

    #[test]
    fn test_validate_private_192_168_rejected() {
        assert!(validate_webhook_url("https://192.168.0.1/webhook").is_err());
        assert!(validate_webhook_url("https://192.168.255.255/webhook").is_err());
        // 192.169.x.x is NOT private
        assert!(validate_webhook_url("https://192.169.0.1/webhook").is_ok());
    }

    #[test]
    fn test_validate_loopback_ipv4_rejected() {
        assert!(validate_webhook_url("https://127.0.0.1/webhook").is_err());
        assert!(validate_webhook_url("https://127.0.0.2/webhook").is_err());
        assert!(validate_webhook_url("https://127.255.255.255/webhook").is_err());
    }

    #[test]
    fn test_validate_loopback_ipv6_rejected() {
        assert!(validate_webhook_url("https://[::1]/webhook").is_err());
    }

    #[test]
    fn test_validate_localhost_rejected() {
        assert!(validate_webhook_url("https://localhost/webhook").is_err());
        assert!(validate_webhook_url("https://localhost:8080/webhook").is_err());
        assert!(validate_webhook_url("https://LOCALHOST/webhook").is_err());
    }

    #[test]
    fn test_validate_invalid_url_rejected() {
        assert!(validate_webhook_url("not-a-url").is_err());
        assert!(validate_webhook_url("").is_err());
        assert!(validate_webhook_url("://missing-scheme").is_err());
    }

    // ── Push notification auth header tests ───────────────────────────────

    fn make_status_event() -> TaskStatusUpdateEvent {
        use a2a_protocol_types::task::{ContextId, TaskId, TaskState, TaskStatus};
        TaskStatusUpdateEvent {
            task_id: TaskId("task-1".to_string()),
            context_id: ContextId("ctx-1".to_string()),
            status: TaskStatus::new(TaskState::Working),
            metadata: None,
        }
    }

    fn make_artifact_event() -> TaskArtifactUpdateEvent {
        use a2a_protocol_types::artifact::{Artifact, ArtifactId};
        use a2a_protocol_types::task::{ContextId, TaskId};
        TaskArtifactUpdateEvent {
            task_id: TaskId("task-1".to_string()),
            context_id: ContextId("ctx-1".to_string()),
            artifact: Artifact {
                id: ArtifactId::new("art-1"),
                name: None,
                description: None,
                parts: vec![],
                metadata: None,
                extensions: None,
            },
            metadata: None,
            append: None,
            last_chunk: None,
        }
    }

    #[tokio::test]
    async fn test_noop_sender_accepts_config_with_neither() {
        let sender = NoOpPushNotificationSender;
        let config = TaskPushNotificationConfig::new("task-1", "https://example.com/hook");
        let event = make_status_event();
        assert!(
            sender.send_status_update("https://example.com/hook", &event, &config).await.is_ok()
        );
    }

    #[tokio::test]
    async fn test_noop_sender_accepts_config_with_bearer_only() {
        let sender = NoOpPushNotificationSender;
        let mut config = TaskPushNotificationConfig::new("task-1", "https://example.com/hook");
        config.authentication = Some(AuthenticationInfo {
            scheme: "bearer".to_string(),
            credentials: "my-token".to_string(),
        });
        let event = make_status_event();
        assert!(
            sender.send_status_update("https://example.com/hook", &event, &config).await.is_ok()
        );
    }

    #[tokio::test]
    async fn test_noop_sender_accepts_config_with_token_only() {
        let sender = NoOpPushNotificationSender;
        let mut config = TaskPushNotificationConfig::new("task-1", "https://example.com/hook");
        config.token = Some("notification-secret".to_string());
        let event = make_artifact_event();
        assert!(
            sender.send_artifact_update("https://example.com/hook", &event, &config).await.is_ok()
        );
    }

    #[tokio::test]
    async fn test_noop_sender_accepts_config_with_both() {
        let sender = NoOpPushNotificationSender;
        let mut config = TaskPushNotificationConfig::new("task-1", "https://example.com/hook");
        config.authentication = Some(AuthenticationInfo {
            scheme: "bearer".to_string(),
            credentials: "my-token".to_string(),
        });
        config.token = Some("notification-secret".to_string());
        let event = make_status_event();
        assert!(
            sender.send_status_update("https://example.com/hook", &event, &config).await.is_ok()
        );
    }
}
