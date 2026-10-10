//! Webhook types and signature verification for Managed Agents.
//!
//! Webhooks notify you of major state changes (session status, vault events)
//! without polling. Webhook events return the event type and ID — fetch the
//! full object with a GET call after receiving the notification.
//!
//! Deliveries are signed with [Standard Webhooks](https://www.standardwebhooks.com/)
//! (`webhook-id`, `webhook-timestamp`, `webhook-signature`); [`WebhookVerifier`]
//! checks them.
//!
//! See: <https://platform.claude.com/docs/en/managed-agents/webhooks>

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

// ─── Webhook Event Types ─────────────────────────────────────────────────────

/// A webhook event delivered to your endpoint.
///
/// The payload contains the event type and resource ID. Fetch the full object
/// via a GET call after receiving the notification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebhookEvent {
    /// Always `"event"`.
    #[serde(rename = "type")]
    pub event_type: String,
    /// Unique event ID (same across retries).
    pub id: String,
    /// ISO 8601 timestamp of when the event was created.
    pub created_at: String,
    /// The event data containing type, resource ID, and org/workspace context.
    pub data: WebhookEventData,
}

/// The data payload within a webhook event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebhookEventData {
    /// The event type (e.g., `"session.status_idled"`, `"vault.created"`).
    #[serde(rename = "type")]
    pub event_type: String,
    /// The resource ID (session ID, vault ID, or credential ID).
    pub id: String,
    /// Organization ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<String>,
    /// Workspace ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// Additional fields.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

// ─── Webhook Signature Verification ──────────────────────────────────────────

/// Error returned when webhook signature verification fails.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum WebhookVerifyError {
    /// The signing secret is not a base64 key, optionally `whsec_`-prefixed.
    InvalidSecret(String),
    /// A required header (`webhook-id`, `webhook-timestamp`, `webhook-signature`)
    /// is absent or not valid UTF-8.
    MissingHeader(&'static str),
    /// The timestamp or signature header is malformed.
    InvalidSignature(String),
    /// No `v1` signature in the header matches the payload.
    SignatureMismatch,
    /// The delivery timestamp is older than the tolerance (replay protection).
    TimestampExpired {
        /// Age of the delivery in seconds.
        age_seconds: u64,
        /// Maximum allowed age in seconds.
        max_age_seconds: u64,
    },
    /// The delivery timestamp is further in the future than the tolerance.
    TimestampInFuture {
        /// How far ahead of the local clock the timestamp is, in seconds.
        skew_seconds: u64,
        /// Maximum allowed skew in seconds.
        max_skew_seconds: u64,
    },
    /// Failed to parse the webhook payload.
    ParseError(String),
}

impl std::fmt::Display for WebhookVerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSecret(msg) => write!(
                f,
                "invalid webhook secret: {msg}; use the whsec_ secret shown when the endpoint was created"
            ),
            Self::MissingHeader(name) => write!(
                f,
                "missing webhook header `{name}`; pass the delivery's headers through unchanged"
            ),
            Self::InvalidSignature(msg) => write!(f, "invalid signature header: {msg}"),
            Self::SignatureMismatch => write!(
                f,
                "webhook signature does not match payload; verify the raw request body, not re-serialized JSON"
            ),
            Self::TimestampExpired { age_seconds, max_age_seconds } => {
                write!(f, "webhook delivery is {age_seconds}s old (max {max_age_seconds}s)")
            }
            Self::TimestampInFuture { skew_seconds, max_skew_seconds } => write!(
                f,
                "webhook timestamp is {skew_seconds}s in the future (max {max_skew_seconds}s); check the local clock"
            ),
            Self::ParseError(msg) => write!(f, "failed to parse webhook payload: {msg}"),
        }
    }
}

impl std::error::Error for WebhookVerifyError {}

/// Default tolerance between a delivery's `webhook-timestamp` and the local clock.
pub const DEFAULT_WEBHOOK_TOLERANCE: std::time::Duration = std::time::Duration::from_secs(300);

/// The three Standard Webhooks headers carried by every delivery.
///
/// # Example
///
/// ```
/// use adk_anthropic::managed_agents::WebhookHeaders;
/// use reqwest::header::{HeaderMap, HeaderValue};
///
/// let mut headers = HeaderMap::new();
/// headers.insert("webhook-id", HeaderValue::from_static("whe_01"));
/// headers.insert("webhook-timestamp", HeaderValue::from_static("1760000000"));
/// headers.insert("webhook-signature", HeaderValue::from_static("v1,c2lnbmF0dXJl"));
/// let parsed = WebhookHeaders::from_header_map(&headers)?;
/// assert_eq!(parsed.id, "whe_01");
/// # Ok::<(), adk_anthropic::managed_agents::WebhookVerifyError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebhookHeaders<'a> {
    /// The `webhook-id` header: the event ID, identical across retries.
    pub id: &'a str,
    /// The `webhook-timestamp` header: Unix seconds when this attempt was signed.
    pub timestamp: &'a str,
    /// The `webhook-signature` header: space-separated `v1,<base64>` signatures.
    pub signature: &'a str,
}

impl<'a> WebhookHeaders<'a> {
    /// Read the three headers from an HTTP header map (case-insensitive).
    ///
    /// # Errors
    ///
    /// Returns [`WebhookVerifyError::MissingHeader`] when a header is absent or not
    /// valid UTF-8.
    pub fn from_header_map(
        headers: &'a reqwest::header::HeaderMap,
    ) -> std::result::Result<Self, WebhookVerifyError> {
        let header = |name: &'static str| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .ok_or(WebhookVerifyError::MissingHeader(name))
        };
        Ok(Self {
            id: header("webhook-id")?,
            timestamp: header("webhook-timestamp")?,
            signature: header("webhook-signature")?,
        })
    }
}

/// Verifies Managed Agents webhook deliveries.
///
/// Implements [Standard Webhooks](https://www.standardwebhooks.com/): the
/// signature is HMAC-SHA256, keyed with the base64-decoded secret, over
/// `{webhook-id}.{webhook-timestamp}.{body}`; the `webhook-signature` header
/// carries one or more space-separated `v1,<base64>` values, any of which may
/// match (signing keys rotate); and the timestamp must be within the tolerance
/// of the local clock in either direction.
///
/// # Example
///
/// ```
/// use adk_anthropic::managed_agents::{WebhookHeaders, WebhookVerifier, WebhookVerifyError};
///
/// let verifier = WebhookVerifier::new("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw")?;
/// let headers = WebhookHeaders { id: "whe_01", timestamp: "0", signature: "v1,AAAA" };
/// // A delivery signed in 1970 is far outside the five-minute tolerance.
/// assert!(matches!(
///     verifier.verify("{}", &headers),
///     Err(WebhookVerifyError::TimestampExpired { .. })
/// ));
/// # Ok::<(), WebhookVerifyError>(())
/// ```
#[derive(Clone)]
pub struct WebhookVerifier {
    key: Vec<u8>,
    tolerance: std::time::Duration,
}

impl std::fmt::Debug for WebhookVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookVerifier")
            .field("key", &"[REDACTED]")
            .field("tolerance", &self.tolerance)
            .finish()
    }
}

impl WebhookVerifier {
    /// Create a verifier from the endpoint's signing secret.
    ///
    /// The secret is base64, optionally prefixed with `whsec_` as the Console shows it.
    ///
    /// # Errors
    ///
    /// Returns [`WebhookVerifyError::InvalidSecret`] when the secret is empty or
    /// not valid base64.
    pub fn new(signing_secret: &str) -> std::result::Result<Self, WebhookVerifyError> {
        use base64::Engine;
        let encoded = signing_secret.strip_prefix("whsec_").unwrap_or(signing_secret);
        let key = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|e| WebhookVerifyError::InvalidSecret(format!("base64 decode failed: {e}")))?;
        if key.is_empty() {
            return Err(WebhookVerifyError::InvalidSecret("the secret is empty".to_string()));
        }
        Ok(Self { key, tolerance: DEFAULT_WEBHOOK_TOLERANCE })
    }

    /// Set the accepted distance between `webhook-timestamp` and the local clock
    /// (default [`DEFAULT_WEBHOOK_TOLERANCE`], five minutes).
    #[must_use]
    pub fn with_tolerance(mut self, tolerance: std::time::Duration) -> Self {
        self.tolerance = tolerance;
        self
    }

    /// Verify a delivery against the current clock and parse its event.
    ///
    /// `payload` must be the raw request body: re-serialized JSON changes the bytes
    /// and fails verification.
    ///
    /// # Errors
    ///
    /// Returns a [`WebhookVerifyError`] when a header is malformed, the timestamp is
    /// outside the tolerance, no signature matches, or the payload is not an event.
    pub fn verify(
        &self,
        payload: &str,
        headers: &WebhookHeaders<'_>,
    ) -> std::result::Result<WebhookEvent, WebhookVerifyError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.verify_at(payload, headers, now)
    }

    fn verify_at(
        &self,
        payload: &str,
        headers: &WebhookHeaders<'_>,
        now: u64,
    ) -> std::result::Result<WebhookEvent, WebhookVerifyError> {
        use base64::Engine;

        let timestamp: u64 = headers.timestamp.trim().parse().map_err(|_| {
            WebhookVerifyError::InvalidSignature(
                "webhook-timestamp is not an integer number of seconds".to_string(),
            )
        })?;
        let tolerance = self.tolerance.as_secs();
        if now > timestamp && now - timestamp > tolerance {
            return Err(WebhookVerifyError::TimestampExpired {
                age_seconds: now - timestamp,
                max_age_seconds: tolerance,
            });
        }
        if timestamp > now && timestamp - now > tolerance {
            return Err(WebhookVerifyError::TimestampInFuture {
                skew_seconds: timestamp - now,
                max_skew_seconds: tolerance,
            });
        }

        let mut mac = HmacSha256::new_from_slice(&self.key)
            .map_err(|e| WebhookVerifyError::InvalidSecret(format!("HMAC init failed: {e}")))?;
        mac.update(headers.id.as_bytes());
        mac.update(b".");
        mac.update(headers.timestamp.as_bytes());
        mac.update(b".");
        mac.update(payload.as_bytes());

        let matched = headers
            .signature
            .split_whitespace()
            .filter_map(|entry| entry.strip_prefix("v1,"))
            .filter_map(|signature| {
                base64::engine::general_purpose::STANDARD.decode(signature).ok()
            })
            // `verify_slice` compares in constant time.
            .any(|signature| mac.clone().verify_slice(&signature).is_ok());
        if !matched {
            return Err(WebhookVerifyError::SignatureMismatch);
        }

        serde_json::from_str(payload).map_err(|e| WebhookVerifyError::ParseError(e.to_string()))
    }
}

/// Verify a webhook delivery with the default five-minute tolerance and parse the event.
///
/// Equivalent to `WebhookVerifier::new(signing_secret)?.verify(payload, &headers)`.
///
/// # Arguments
///
/// * `payload` - The raw request body
/// * `headers` - The delivery's `webhook-id`, `webhook-timestamp`, and `webhook-signature`
/// * `signing_secret` - The `whsec_`-prefixed signing secret from the Console
///
/// # Errors
///
/// Returns [`WebhookVerifyError`] if the secret or a header is malformed, the
/// timestamp is outside the tolerance, no signature matches, or parsing fails.
///
/// # Example
///
/// ```
/// use adk_anthropic::managed_agents::{WebhookHeaders, WebhookVerifyError, verify_webhook};
///
/// let headers = WebhookHeaders { id: "whe_01", timestamp: "0", signature: "v1,AAAA" };
/// let result = verify_webhook("{}", headers, "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw");
/// assert!(matches!(result, Err(WebhookVerifyError::TimestampExpired { .. })));
/// ```
pub fn verify_webhook(
    payload: &str,
    headers: WebhookHeaders<'_>,
    signing_secret: &str,
) -> std::result::Result<WebhookEvent, WebhookVerifyError> {
    WebhookVerifier::new(signing_secret)?.verify(payload, &headers)
}

// ─── Webhook Event Type Constants ────────────────────────────────────────────

/// Session webhook event types.
pub mod session_events {
    /// Agent execution kicked off.
    pub const STATUS_RUN_STARTED: &str = "session.status_run_started";
    /// Agent awaiting input.
    pub const STATUS_IDLED: &str = "session.status_idled";
    /// Transient error, retrying.
    pub const STATUS_RESCHEDULED: &str = "session.status_rescheduled";
    /// Terminal error.
    pub const STATUS_TERMINATED: &str = "session.status_terminated";
    /// Multiagent thread opened.
    pub const THREAD_CREATED: &str = "session.thread_created";
    /// Multiagent thread waiting.
    pub const THREAD_IDLED: &str = "session.thread_idled";
    /// Multiagent thread archived.
    pub const THREAD_TERMINATED: &str = "session.thread_terminated";
    /// Outcome evaluation completed.
    pub const OUTCOME_EVALUATION_ENDED: &str = "session.outcome_evaluation_ended";
}

/// Vault webhook event types.
pub mod vault_events {
    /// Vault created.
    pub const VAULT_CREATED: &str = "vault.created";
    /// Vault archived.
    pub const VAULT_ARCHIVED: &str = "vault.archived";
    /// Vault deleted.
    pub const VAULT_DELETED: &str = "vault.deleted";
    /// Credential created.
    pub const CREDENTIAL_CREATED: &str = "vault_credential.created";
    /// Credential archived.
    pub const CREDENTIAL_ARCHIVED: &str = "vault_credential.archived";
    /// Credential deleted.
    pub const CREDENTIAL_DELETED: &str = "vault_credential.deleted";
    /// OAuth refresh failed.
    pub const CREDENTIAL_REFRESH_FAILED: &str = "vault_credential.refresh_failed";
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    const SECRET: &str = "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw";
    const PAYLOAD: &str = r#"{"type":"event","id":"whe_01ABC","created_at":"2026-06-01T00:00:00Z","data":{"type":"session.status_idled","id":"sesn_01XYZ"}}"#;
    const NOW: u64 = 1_760_000_000;

    /// Signs as the Standard Webhooks reference implementation does.
    fn sign(id: &str, timestamp: u64, payload: &str) -> String {
        let key = base64::engine::general_purpose::STANDARD
            .decode(SECRET.trim_start_matches("whsec_"))
            .unwrap();
        let mut mac = HmacSha256::new_from_slice(&key).unwrap();
        mac.update(format!("{id}.{timestamp}.{payload}").as_bytes());
        format!(
            "v1,{}",
            base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
        )
    }

    fn verify(
        timestamp: u64,
        signature: &str,
        now: u64,
    ) -> std::result::Result<WebhookEvent, WebhookVerifyError> {
        let timestamp = timestamp.to_string();
        let headers = WebhookHeaders { id: "whe_01ABC", timestamp: &timestamp, signature };
        WebhookVerifier::new(SECRET).unwrap().verify_at(PAYLOAD, &headers, now)
    }

    #[test]
    fn standard_webhooks_reference_vector_verifies() {
        // Test vector published by the Standard Webhooks specification.
        let headers = WebhookHeaders {
            id: "msg_p5jXN8AQM9LWM0D4loKWxJek",
            timestamp: "1614265330",
            signature: "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE=",
        };
        let error = WebhookVerifier::new(SECRET)
            .unwrap()
            .verify_at(r#"{"test": 2432232314}"#, &headers, 1_614_265_330)
            .expect_err("the vector's body is not a Managed Agents event");
        assert!(matches!(error, WebhookVerifyError::ParseError(_)), "{error:?}");
    }

    #[test]
    fn valid_delivery_verifies_and_parses() {
        let event = verify(NOW, &sign("whe_01ABC", NOW, PAYLOAD), NOW).unwrap();
        assert_eq!(event.id, "whe_01ABC");
        assert_eq!(event.data.event_type, "session.status_idled");
        assert_eq!(event.data.id, "sesn_01XYZ");
    }

    #[test]
    fn any_matching_signature_among_several_verifies() {
        let header = format!("v1,AAAA v2,ignored {}", sign("whe_01ABC", NOW, PAYLOAD));
        assert!(verify(NOW, &header, NOW).is_ok());
    }

    #[test]
    fn the_id_and_timestamp_are_part_of_the_signed_content() {
        let other_id = sign("whe_other", NOW, PAYLOAD);
        assert_eq!(verify(NOW, &other_id, NOW), Err(WebhookVerifyError::SignatureMismatch));
        let other_time = sign("whe_01ABC", NOW - 1, PAYLOAD);
        assert_eq!(verify(NOW, &other_time, NOW), Err(WebhookVerifyError::SignatureMismatch));
    }

    #[test]
    fn tampered_payload_fails() {
        let signature = sign("whe_01ABC", NOW, PAYLOAD);
        let headers =
            WebhookHeaders { id: "whe_01ABC", timestamp: "1760000000", signature: &signature };
        let tampered = PAYLOAD.replace("sesn_01XYZ", "sesn_01EVIL");
        assert_eq!(
            WebhookVerifier::new(SECRET).unwrap().verify_at(&tampered, &headers, NOW),
            Err(WebhookVerifyError::SignatureMismatch)
        );
    }

    #[test]
    fn timestamps_outside_the_tolerance_fail_in_both_directions() {
        let old = NOW - 301;
        assert_eq!(
            verify(old, &sign("whe_01ABC", old, PAYLOAD), NOW),
            Err(WebhookVerifyError::TimestampExpired { age_seconds: 301, max_age_seconds: 300 })
        );
        let future = NOW + 301;
        assert_eq!(
            verify(future, &sign("whe_01ABC", future, PAYLOAD), NOW),
            Err(WebhookVerifyError::TimestampInFuture { skew_seconds: 301, max_skew_seconds: 300 })
        );
        let edge = NOW - 300;
        assert!(verify(edge, &sign("whe_01ABC", edge, PAYLOAD), NOW).is_ok());
    }

    #[test]
    fn signatures_in_other_schemes_are_rejected() {
        // The previous, non-standard format: hex HMAC over `v1.{timestamp}.{payload}`.
        let mut mac = HmacSha256::new_from_slice(b"whatever").unwrap();
        mac.update(format!("v1.{NOW}.{PAYLOAD}").as_bytes());
        let legacy = format!("v1,{NOW},{}", hex::encode(mac.finalize().into_bytes()));
        assert_eq!(verify(NOW, &legacy, NOW), Err(WebhookVerifyError::SignatureMismatch));
        assert_eq!(verify(NOW, "", NOW), Err(WebhookVerifyError::SignatureMismatch));
    }

    #[test]
    fn malformed_secret_and_headers_fail() {
        assert!(matches!(
            WebhookVerifier::new("whsec_not base64!"),
            Err(WebhookVerifyError::InvalidSecret(_))
        ));
        assert!(matches!(
            WebhookVerifier::new("whsec_"),
            Err(WebhookVerifyError::InvalidSecret(_))
        ));
        let headers = WebhookHeaders { id: "whe_01ABC", timestamp: "soon", signature: "v1,AAAA" };
        assert!(matches!(
            WebhookVerifier::new(SECRET).unwrap().verify_at(PAYLOAD, &headers, NOW),
            Err(WebhookVerifyError::InvalidSignature(_))
        ));
        let empty = reqwest::header::HeaderMap::new();
        assert_eq!(
            WebhookHeaders::from_header_map(&empty),
            Err(WebhookVerifyError::MissingHeader("webhook-id"))
        );
    }

    #[test]
    fn debug_output_redacts_the_key() {
        let verifier = WebhookVerifier::new(SECRET).unwrap();
        assert!(!format!("{verifier:?}").contains("MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw"));
    }
}
