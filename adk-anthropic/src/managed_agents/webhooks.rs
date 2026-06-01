//! Webhook types and signature verification for Managed Agents.
//!
//! Webhooks notify you of major state changes (session status, vault events)
//! without polling. Webhook events return the event type and ID — fetch the
//! full object with a GET call after receiving the notification.
//!
//! See: <https://platform.claude.com/docs/en/managed-agents/subscribe-to-webhooks>

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
pub enum WebhookVerifyError {
    /// The signing secret is invalid (not whsec_ prefixed or wrong length).
    InvalidSecret(String),
    /// The signature header is missing or malformed.
    InvalidSignature(String),
    /// The signature does not match the payload.
    SignatureMismatch,
    /// The payload timestamp is too old (replay protection).
    TimestampExpired {
        /// Age of the payload in seconds.
        age_seconds: u64,
        /// Maximum allowed age.
        max_age_seconds: u64,
    },
    /// Failed to parse the webhook payload.
    ParseError(String),
}

impl std::fmt::Display for WebhookVerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSecret(msg) => write!(f, "invalid webhook secret: {msg}"),
            Self::InvalidSignature(msg) => write!(f, "invalid signature header: {msg}"),
            Self::SignatureMismatch => write!(f, "webhook signature does not match payload"),
            Self::TimestampExpired { age_seconds, max_age_seconds } => {
                write!(f, "webhook payload is {age_seconds}s old (max {max_age_seconds}s)")
            }
            Self::ParseError(msg) => write!(f, "failed to parse webhook payload: {msg}"),
        }
    }
}

impl std::error::Error for WebhookVerifyError {}

/// Maximum age of a webhook payload before it's considered stale (5 minutes).
const MAX_PAYLOAD_AGE_SECONDS: u64 = 300;

/// Verify a webhook signature and parse the event.
///
/// This validates the `X-Webhook-Signature` header against the payload using
/// the signing secret, checks the timestamp for replay protection, and
/// deserializes the event.
///
/// # Arguments
///
/// * `payload` - The raw request body as a string
/// * `signature_header` - The value of the `X-Webhook-Signature` header
/// * `signing_secret` - The `whsec_`-prefixed signing secret from Console
///
/// # Returns
///
/// The parsed `WebhookEvent` if verification succeeds.
///
/// # Errors
///
/// Returns `WebhookVerifyError` if the signature is invalid, the payload is
/// stale, or parsing fails.
pub fn verify_webhook(
    payload: &str,
    signature_header: &str,
    signing_secret: &str,
) -> std::result::Result<WebhookEvent, WebhookVerifyError> {
    // Extract the secret key from the whsec_ prefix
    let secret_bytes = decode_signing_secret(signing_secret)?;

    // Parse the signature header: "v1,<timestamp>,<signature>"
    let parts: Vec<&str> = signature_header.split(',').collect();
    if parts.len() != 3 || parts[0] != "v1" {
        return Err(WebhookVerifyError::InvalidSignature(
            "expected format: v1,<timestamp>,<signature>".to_string(),
        ));
    }

    let timestamp_str = parts[1];
    let provided_signature = parts[2];

    // Verify timestamp freshness
    let timestamp: u64 = timestamp_str.parse().map_err(|_| {
        WebhookVerifyError::InvalidSignature("timestamp is not a valid integer".to_string())
    })?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    if now > timestamp && (now - timestamp) > MAX_PAYLOAD_AGE_SECONDS {
        return Err(WebhookVerifyError::TimestampExpired {
            age_seconds: now - timestamp,
            max_age_seconds: MAX_PAYLOAD_AGE_SECONDS,
        });
    }

    // Compute expected signature: HMAC-SHA256(secret, "v1.<timestamp>.<payload>")
    let signed_content = format!("v1.{timestamp_str}.{payload}");
    let mut mac = HmacSha256::new_from_slice(&secret_bytes)
        .map_err(|e| WebhookVerifyError::InvalidSecret(format!("HMAC init failed: {e}")))?;
    mac.update(signed_content.as_bytes());
    let expected = mac.finalize().into_bytes();

    // Compare signatures (constant-time via hex comparison)
    let expected_hex = hex::encode(expected);
    if !constant_time_eq(expected_hex.as_bytes(), provided_signature.as_bytes()) {
        return Err(WebhookVerifyError::SignatureMismatch);
    }

    // Parse the event
    let event: WebhookEvent = serde_json::from_str(payload)
        .map_err(|e| WebhookVerifyError::ParseError(format!("{e}")))?;

    Ok(event)
}

/// Decode a `whsec_`-prefixed signing secret into raw bytes.
fn decode_signing_secret(secret: &str) -> std::result::Result<Vec<u8>, WebhookVerifyError> {
    let encoded = secret.strip_prefix("whsec_").ok_or_else(|| {
        WebhookVerifyError::InvalidSecret("secret must start with 'whsec_'".to_string())
    })?;

    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|e| WebhookVerifyError::InvalidSecret(format!("base64 decode failed: {e}")))
}

/// Constant-time byte comparison to prevent timing attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut result = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }
    result == 0
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

    fn make_test_secret() -> (String, Vec<u8>) {
        use base64::Engine;
        let key = b"test-webhook-secret-32-bytes-ok!"; // 32 bytes
        let encoded = base64::engine::general_purpose::STANDARD.encode(key);
        (format!("whsec_{encoded}"), key.to_vec())
    }

    fn sign_payload(payload: &str, timestamp: u64, key: &[u8]) -> String {
        let signed_content = format!("v1.{timestamp}.{payload}");
        let mut mac = HmacSha256::new_from_slice(key).unwrap();
        mac.update(signed_content.as_bytes());
        let signature = hex::encode(mac.finalize().into_bytes());
        format!("v1,{timestamp},{signature}")
    }

    #[test]
    fn test_verify_valid_webhook() {
        let (secret, key) = make_test_secret();
        let payload = r#"{"type":"event","id":"event_01ABC","created_at":"2026-06-01T00:00:00Z","data":{"type":"session.status_idled","id":"sesn_01XYZ"}}"#;
        let now =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let sig = sign_payload(payload, now, &key);

        let event = verify_webhook(payload, &sig, &secret).unwrap();
        assert_eq!(event.id, "event_01ABC");
        assert_eq!(event.data.event_type, "session.status_idled");
        assert_eq!(event.data.id, "sesn_01XYZ");
    }

    #[test]
    fn test_verify_invalid_signature() {
        let (secret, _key) = make_test_secret();
        let payload = r#"{"type":"event","id":"event_01ABC","created_at":"2026-06-01T00:00:00Z","data":{"type":"session.status_idled","id":"sesn_01XYZ"}}"#;
        let now =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let bad_sig = format!("v1,{now},deadbeef");

        let result = verify_webhook(payload, &bad_sig, &secret);
        assert!(matches!(result, Err(WebhookVerifyError::SignatureMismatch)));
    }

    #[test]
    fn test_verify_expired_timestamp() {
        let (secret, key) = make_test_secret();
        let payload = r#"{"type":"event","id":"event_01ABC","created_at":"2026-06-01T00:00:00Z","data":{"type":"session.status_idled","id":"sesn_01XYZ"}}"#;
        let old_timestamp =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
                - 600; // 10 minutes ago
        let sig = sign_payload(payload, old_timestamp, &key);

        let result = verify_webhook(payload, &sig, &secret);
        assert!(matches!(result, Err(WebhookVerifyError::TimestampExpired { .. })));
    }

    #[test]
    fn test_verify_invalid_secret_prefix() {
        let result = verify_webhook("payload", "v1,123,abc", "not_whsec_prefix");
        assert!(matches!(result, Err(WebhookVerifyError::InvalidSecret(_))));
    }

    #[test]
    fn test_verify_malformed_signature_header() {
        let (secret, _) = make_test_secret();
        let result = verify_webhook("payload", "bad-header", &secret);
        assert!(matches!(result, Err(WebhookVerifyError::InvalidSignature(_))));
    }
}
