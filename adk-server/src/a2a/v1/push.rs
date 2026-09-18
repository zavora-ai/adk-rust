//! A2A v1.0.0 push notification sender.
//!
//! Defines the [`PushNotificationSender`] trait for delivering task updates
//! to client-registered webhook endpoints. Includes [`HttpPushNotificationSender`]
//! with retry logic and SSRF validation, and [`NoOpPushNotificationSender`] for
//! development and testing.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
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

/// Upper bound on one webhook delivery attempt, from connect to response.
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(10);

/// Upper bound on establishing the connection for one delivery attempt.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The client [`HttpPushNotificationSender::new`] uses.
///
/// Redirects are not followed: [`validate_webhook_url`] checks the registered URL
/// only, and a redirect would let a public endpoint bounce the request to an
/// internal one. Timeouts keep an unresponsive webhook from holding a delivery
/// task open.
fn push_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(DELIVERY_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("push notification HTTP client must build with the default TLS backend")
}

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
/// webhook URLs with [`validate_webhook_url`] before every delivery (SSRF
/// prevention).
pub struct HttpPushNotificationSender {
    client: reqwest::Client,
}

impl HttpPushNotificationSender {
    /// Creates a new sender whose client does not follow redirects, gives up on
    /// an attempt after 10 seconds, and on a connection after 5 seconds.
    pub fn new() -> Self {
        Self { client: push_http_client() }
    }

    /// Creates a new sender with a custom `reqwest::Client`.
    ///
    /// The client should disable redirects (`reqwest::redirect::Policy::none()`)
    /// and set a timeout: URL validation covers the registered URL only, so a
    /// followed redirect can reach an address the validation would have refused.
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
                // `credentials` became Option<String> in a2a-protocol-types 0.12:
                // omit the header entirely rather than sending "Bearer None".
                if let Some(ref credentials) = auth.credentials {
                    request = request.header("Authorization", format!("Bearer {credentials}"));
                }
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
/// Rejects URLs whose host is a literal address in a non-public range, or a
/// `localhost` name:
///
/// | Range | Purpose |
/// |-------|---------|
/// | `0.0.0.0/8`, `::` | "this network" / unspecified |
/// | `127.0.0.0/8`, `::1` | loopback |
/// | `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16` | private |
/// | `100.64.0.0/10` | carrier-grade NAT shared space |
/// | `169.254.0.0/16`, `fe80::/10` | link-local, including cloud metadata endpoints |
/// | `fc00::/7`, `fec0::/10` | IPv6 unique-local and deprecated site-local |
/// | `224.0.0.0/4`, `255.255.255.255`, `ff00::/8` | multicast and broadcast |
/// | `::ffff:0:0/96`, `::/96`, `64:ff9b::/96` | IPv6 forms embedding any IPv4 address above |
///
/// The URL parser normalizes alternative IPv4 spellings (`0x7f.1`, `2130706433`)
/// before the check. Host names other than `localhost` are not resolved here;
/// put the sender behind an egress proxy when webhook hosts are untrusted.
///
/// # Example
///
/// ```rust
/// use adk_server::a2a::v1::push::validate_webhook_url;
///
/// assert!(validate_webhook_url("https://hooks.example.com/a2a").is_ok());
/// assert!(validate_webhook_url("http://169.254.169.254/latest/meta-data").is_err());
/// ```
///
/// # Errors
///
/// Returns `A2aError::InvalidParams` for an unparseable URL, a URL without a host,
/// or a host in one of the ranges above.
pub fn validate_webhook_url(url: &str) -> Result<(), A2aError> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| A2aError::InvalidParams { message: format!("invalid webhook URL: {e}") })?;

    let host = parsed.host_str().ok_or_else(|| A2aError::InvalidParams {
        message: "webhook URL has no host".to_string(),
    })?;

    // IPv6 hosts are serialized in brackets, e.g. `[::1]`.
    let unbracketed = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    if let Ok(ip) = unbracketed.parse::<IpAddr>() {
        if is_non_public(ip) {
            return Err(A2aError::InvalidParams {
                message: format!("webhook URL must not target a non-public address: {ip}"),
            });
        }
        return Ok(());
    }

    let name = host.trim_end_matches('.').to_ascii_lowercase();
    if name == "localhost" || name.ends_with(".localhost") {
        return Err(A2aError::InvalidParams {
            message: "webhook URL must not target localhost".to_string(),
        });
    }

    Ok(())
}

/// Returns `true` for an address a webhook must not target; see [`validate_webhook_url`].
fn is_non_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_non_public_v4(v4),
        IpAddr::V6(v6) => is_non_public_v6(v6),
    }
}

fn is_non_public_v4(v4: Ipv4Addr) -> bool {
    let [a, b, ..] = v4.octets();
    a == 0
        || v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || (a == 100 && (64..=127).contains(&b))
        || v4.is_multicast()
        || v4.is_broadcast()
}

fn is_non_public_v6(v6: Ipv6Addr) -> bool {
    let segments = v6.segments();
    // NAT64 (64:ff9b::/96) carries an IPv4 address in its low 32 bits.
    let nat64 = segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0];
    // `to_ipv4` covers both IPv4-mapped (::ffff:a.b.c.d) and IPv4-compatible (::a.b.c.d).
    let embedded = v6.to_ipv4().or_else(|| {
        nat64.then(|| {
            let [.., hi, lo] = segments;
            Ipv4Addr::from((u32::from(hi) << 16) | u32::from(lo))
        })
    });
    if let Some(v4) = embedded {
        return is_non_public_v4(v4);
    }
    v6.is_unspecified()
        || v6.is_loopback()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] & 0xffc0) == 0xfec0
        || v6.is_multicast()
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

    /// Every form here reaches a host-local, internal, or metadata address.
    #[test]
    fn test_validate_rejects_every_non_public_form() {
        let blocked = [
            // 0.0.0.0/8 — routes to the local host on most stacks
            "http://0.0.0.0/hook",
            "http://0.1.2.3/hook",
            // 169.254.0.0/16 — link-local, including cloud metadata
            "http://169.254.169.254/latest/meta-data",
            "http://169.254.0.1/hook",
            // 100.64.0.0/10 — carrier-grade NAT shared space
            "http://100.64.0.1/hook",
            "http://100.127.255.254/hook",
            // Multicast and broadcast
            "http://224.0.0.1/hook",
            "http://255.255.255.255/hook",
            // Alternative IPv4 spellings the URL parser normalizes
            "http://2130706433/hook",
            "http://0x7f.1/hook",
            "http://0177.0.0.1/hook",
            // IPv6 unspecified, unique-local (fc00::/7), link-local (fe80::/10), site-local
            "http://[::]/hook",
            "http://[fc00::1]/hook",
            "http://[fd12:3456:789a::1]/hook",
            "http://[fe80::1]/hook",
            "http://[febf::1]/hook",
            "http://[fec0::1]/hook",
            "http://[ff02::1]/hook",
            // IPv4-mapped, IPv4-compatible, and NAT64 forms of blocked IPv4 ranges
            "http://[::ffff:127.0.0.1]/hook",
            "http://[::ffff:169.254.169.254]/hook",
            "http://[::ffff:10.0.0.1]/hook",
            "http://[::ffff:7f00:1]/hook",
            "http://[::127.0.0.1]/hook",
            "http://[64:ff9b::a9fe:a9fe]/hook",
            // localhost names
            "http://localhost./hook",
            "http://api.localhost/hook",
        ];
        for url in blocked {
            assert!(validate_webhook_url(url).is_err(), "{url} must be rejected");
        }
    }

    #[test]
    fn test_validate_accepts_public_neighbours_of_blocked_ranges() {
        let allowed = [
            "http://100.63.255.255/hook",
            "http://100.128.0.1/hook",
            "http://169.253.0.1/hook",
            "http://1.0.0.1/hook",
            "http://[2001:4860:4860::8888]/hook",
            "http://[::ffff:8.8.8.8]/hook",
            "http://[64:ff9b::808:808]/hook",
            "http://[fbff::1]/hook",
            "https://notlocalhost.example/hook",
        ];
        for url in allowed {
            assert!(validate_webhook_url(url).is_ok(), "{url} must be accepted");
        }
    }

    #[tokio::test]
    async fn test_push_client_does_not_follow_redirects() {
        use std::io::{Read, Write};

        // The redirect target stands in for an internal service; nothing may connect to it.
        let target = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        target.set_nonblocking(true).unwrap();
        let target_addr = target.local_addr().unwrap();

        let redirector = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let redirector_addr = redirector.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = redirector.accept().unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{target_addr}/internal\r\n\
                 Content-Length: 0\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let response = tokio::time::timeout(
            Duration::from_secs(5),
            push_http_client()
                .post(format!("http://{redirector_addr}/hook"))
                .json(&serde_json::json!({}))
                .send(),
        )
        .await
        .expect("the redirect must not be followed to a target that never answers")
        .unwrap();
        server.join().unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::TEMPORARY_REDIRECT);
        assert!(
            matches!(target.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
            "the redirect target must never be contacted"
        );
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
            credentials: Some("my-token".to_string()),
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
            credentials: Some("my-token".to_string()),
        });
        config.token = Some("notification-secret".to_string());
        let event = make_status_event();
        assert!(
            sender.send_status_update("https://example.com/hook", &event, &config).await.is_ok()
        );
    }
}
