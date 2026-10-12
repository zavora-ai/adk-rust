//! [Standard Webhooks](https://www.standardwebhooks.com/) signing and verification.
//!
//! Every inbound wake-up endpoint and every outbound delivery in ADK-Rust uses this one
//! implementation (feature `webhooks`):
//!
//! | Header | Value |
//! |--------|-------|
//! | `webhook-id` | The message identifier; identical on every retry of one message |
//! | `webhook-timestamp` | Unix seconds when this attempt was signed |
//! | `webhook-signature` | Space-separated `v1,<base64>` signatures; any one may match |
//!
//! Each signature is HMAC-SHA256, keyed with the secret's bytes, over
//! `{webhook-id}.{webhook-timestamp}.{raw body}`.
//!
//! | Guarantee | Mechanism |
//! |-----------|-----------|
//! | Authenticity | [`WebhookVerifier`] recomputes the HMAC and compares in constant time |
//! | Freshness | The timestamp must be within the tolerance (default five minutes) of the local clock, in either direction |
//! | Key rotation | The verifier holds several secrets and the header carries several signatures; one match from any pair is enough |
//! | Replay protection | A replay outside the tolerance fails the timestamp check; inside it, [`WebhookReplayGuard`] remembers the `webhook-id` for twice the tolerance. The run-store ingress uses the `webhook-id` as the run's idempotency key instead |
//!
//! # Example
//!
//! ```rust
//! use adk_core::webhook::{WebhookHeaders, WebhookSecret, WebhookSigner, WebhookVerifier};
//!
//! let secret = WebhookSecret::from_base64("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw")?;
//! let signer = WebhookSigner::new(secret.clone());
//! let body = br#"{"event":"invoice.paid"}"#;
//! let signed = signer.sign_now("msg_1", body);
//!
//! let verifier = WebhookVerifier::new(secret);
//! let verified = verifier.verify(&signed.as_headers(), body)?;
//! assert_eq!(verified.id, "msg_1");
//! # Ok::<(), adk_core::webhook::WebhookError>(())
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::{AdkError, Clock, ErrorCategory, ErrorComponent, SystemClock};

type HmacSha256 = Hmac<Sha256>;

/// The `webhook-id` header name.
pub const WEBHOOK_ID_HEADER: &str = "webhook-id";
/// The `webhook-timestamp` header name.
pub const WEBHOOK_TIMESTAMP_HEADER: &str = "webhook-timestamp";
/// The `webhook-signature` header name.
pub const WEBHOOK_SIGNATURE_HEADER: &str = "webhook-signature";
/// Default accepted distance between `webhook-timestamp` and the local clock.
pub const DEFAULT_WEBHOOK_TOLERANCE: Duration = Duration::from_secs(300);

/// Why a webhook secret, signature, or delivery was rejected.
///
/// The reason is for logs. An HTTP endpoint answers every variant with a bare `401`, so
/// a caller cannot probe which part of a credential was wrong.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum WebhookError {
    /// The secret is empty or not valid base64.
    #[error(
        "invalid webhook secret: {0}; pass the base64 secret, optionally prefixed with `whsec_`"
    )]
    InvalidSecret(String),
    /// A required header is absent or not valid UTF-8.
    #[error("missing webhook header `{0}`; forward the delivery's headers unchanged")]
    MissingHeader(&'static str),
    /// A header is present but malformed.
    #[error("malformed `{header}` header: {reason}")]
    MalformedHeader {
        /// The header name.
        header: &'static str,
        /// What is wrong with it.
        reason: String,
    },
    /// No `v1` signature matches any configured secret.
    #[error(
        "webhook signature does not match; verify the raw request body, not re-serialized JSON, \
         and check the signing secret"
    )]
    SignatureMismatch,
    /// The delivery was signed longer ago than the tolerance.
    #[error("webhook delivery is {age_seconds}s old (max {max_age_seconds}s)")]
    TimestampExpired {
        /// Age of the delivery in seconds.
        age_seconds: u64,
        /// Accepted age in seconds.
        max_age_seconds: u64,
    },
    /// The delivery timestamp is further in the future than the tolerance.
    #[error(
        "webhook timestamp is {skew_seconds}s in the future (max {max_skew_seconds}s); check the \
         sender's clock"
    )]
    TimestampInFuture {
        /// How far ahead of the local clock the timestamp is, in seconds.
        skew_seconds: u64,
        /// Accepted skew in seconds.
        max_skew_seconds: u64,
    },
    /// A delivery with this `webhook-id` was already accepted.
    #[error("webhook `{0}` was already received; a replay is ignored")]
    Replayed(String),
}

impl WebhookError {
    /// A stable code for logs and metrics, such as `webhook.signature_mismatch`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidSecret(_) => "webhook.invalid_secret",
            Self::MissingHeader(_) => "webhook.missing_header",
            Self::MalformedHeader { .. } => "webhook.malformed_header",
            Self::SignatureMismatch => "webhook.signature_mismatch",
            Self::TimestampExpired { .. } => "webhook.timestamp_expired",
            Self::TimestampInFuture { .. } => "webhook.timestamp_in_future",
            Self::Replayed(_) => "webhook.replayed",
        }
    }
}

impl From<WebhookError> for AdkError {
    fn from(error: WebhookError) -> Self {
        let category = match error {
            WebhookError::InvalidSecret(_) => ErrorCategory::InvalidInput,
            WebhookError::Replayed(_) => ErrorCategory::Conflict,
            WebhookError::MissingHeader(_)
            | WebhookError::MalformedHeader { .. }
            | WebhookError::SignatureMismatch
            | WebhookError::TimestampExpired { .. }
            | WebhookError::TimestampInFuture { .. } => ErrorCategory::Unauthorized,
        };
        AdkError::new(ErrorComponent::Server, category, error.code(), error.to_string())
    }
}

/// A webhook signing secret.
///
/// `Debug` redacts the key.
///
/// # Example
///
/// ```rust
/// use adk_core::webhook::WebhookSecret;
///
/// let secret = WebhookSecret::from_base64("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw")?;
/// assert_eq!(secret.to_whsec(), "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw");
/// assert!(format!("{secret:?}").contains("REDACTED"));
/// # Ok::<(), adk_core::webhook::WebhookError>(())
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct WebhookSecret {
    key: Arc<[u8]>,
}

impl std::fmt::Debug for WebhookSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookSecret").field("key", &"[REDACTED]").finish()
    }
}

impl WebhookSecret {
    /// Decodes a base64 secret, optionally prefixed with `whsec_`.
    ///
    /// # Errors
    ///
    /// Returns [`WebhookError::InvalidSecret`] when the secret is not base64 or decodes
    /// to no bytes.
    pub fn from_base64(secret: &str) -> Result<Self, WebhookError> {
        let encoded = secret.trim().strip_prefix("whsec_").unwrap_or(secret.trim());
        let key = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|e| WebhookError::InvalidSecret(format!("base64 decode failed: {e}")))?;
        Self::from_bytes(key)
    }

    /// Uses `key` as the HMAC key as is.
    ///
    /// # Errors
    ///
    /// Returns [`WebhookError::InvalidSecret`] when `key` is empty.
    pub fn from_bytes(key: impl Into<Vec<u8>>) -> Result<Self, WebhookError> {
        let key = key.into();
        if key.is_empty() {
            return Err(WebhookError::InvalidSecret("the secret is empty".to_string()));
        }
        Ok(Self { key: key.into() })
    }

    /// The secret in its `whsec_<base64>` form, as receivers configure it.
    pub fn to_whsec(&self) -> String {
        format!("whsec_{}", base64::engine::general_purpose::STANDARD.encode(&self.key))
    }

    fn mac(&self, id: &str, timestamp: &str, body: &[u8]) -> HmacSha256 {
        // HMAC accepts keys of any length, and `from_bytes` refuses an empty one.
        let mut mac = HmacSha256::new_from_slice(&self.key).expect("HMAC accepts any key length");
        mac.update(id.as_bytes());
        mac.update(b".");
        mac.update(timestamp.as_bytes());
        mac.update(b".");
        mac.update(body);
        mac
    }

    /// The `v1,<base64>` signature of one message.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::webhook::WebhookSecret;
    ///
    /// // The Standard Webhooks reference test vector.
    /// let secret = WebhookSecret::from_base64("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw")?;
    /// let signature = secret.signature("msg_p5jXN8AQM9LWM0D4loKWxJek", 1614265330, br#"{"test": 2432232314}"#);
    /// assert_eq!(signature, "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE=");
    /// # Ok::<(), adk_core::webhook::WebhookError>(())
    /// ```
    pub fn signature(&self, id: &str, timestamp: i64, body: &[u8]) -> String {
        let digest = self.mac(id, &timestamp.to_string(), body).finalize().into_bytes();
        format!("v1,{}", base64::engine::general_purpose::STANDARD.encode(digest))
    }
}

/// The three headers of one signed delivery attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedWebhook {
    /// The `webhook-id` value.
    pub id: String,
    /// The `webhook-timestamp` value, Unix seconds.
    pub timestamp: i64,
    /// The `webhook-signature` value.
    pub signature: String,
}

impl SignedWebhook {
    /// Borrows the values as [`WebhookHeaders`], with the timestamp rendered as text.
    pub fn as_headers(&self) -> WebhookHeaders<'_> {
        WebhookHeaders {
            id: &self.id,
            timestamp: std::borrow::Cow::Owned(self.timestamp.to_string()),
            signature: &self.signature,
        }
    }

    /// The headers as `(name, value)` pairs, ready for an HTTP client.
    pub fn header_pairs(&self) -> [(&'static str, String); 3] {
        [
            (WEBHOOK_ID_HEADER, self.id.clone()),
            (WEBHOOK_TIMESTAMP_HEADER, self.timestamp.to_string()),
            (WEBHOOK_SIGNATURE_HEADER, self.signature.clone()),
        ]
    }
}

/// Signs outbound deliveries.
///
/// During a key rotation, [`with_secret`](Self::with_secret) adds the next secret and
/// every delivery carries one signature per secret, so receivers on either key accept it.
///
/// # Example
///
/// ```rust
/// use adk_core::webhook::{WebhookSecret, WebhookSigner};
///
/// let old = WebhookSecret::from_bytes(b"old-secret-old-secret-old-secret".to_vec())?;
/// let new = WebhookSecret::from_bytes(b"new-secret-new-secret-new-secret".to_vec())?;
/// let signed = WebhookSigner::new(old).with_secret(new).sign("msg_1", 1_760_000_000, b"{}");
/// assert_eq!(signed.signature.split(' ').count(), 2);
/// # Ok::<(), adk_core::webhook::WebhookError>(())
/// ```
#[derive(Debug, Clone)]
pub struct WebhookSigner {
    secrets: Vec<WebhookSecret>,
    clock: Arc<dyn Clock>,
}

impl WebhookSigner {
    /// Creates a signer with one secret that reads the system clock.
    pub fn new(secret: WebhookSecret) -> Self {
        Self { secrets: vec![secret], clock: Arc::new(SystemClock) }
    }

    /// Adds another secret; each delivery carries a signature per secret.
    pub fn with_secret(mut self, secret: WebhookSecret) -> Self {
        self.secrets.push(secret);
        self
    }

    /// Sets the clock [`sign_now`](Self::sign_now) reads.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Signs `body` as message `id` at `timestamp` (Unix seconds).
    pub fn sign(&self, id: &str, timestamp: i64, body: &[u8]) -> SignedWebhook {
        let signature = self
            .secrets
            .iter()
            .map(|secret| secret.signature(id, timestamp, body))
            .collect::<Vec<_>>()
            .join(" ");
        SignedWebhook { id: id.to_string(), timestamp, signature }
    }

    /// Signs `body` as message `id` at the current time.
    pub fn sign_now(&self, id: &str, body: &[u8]) -> SignedWebhook {
        self.sign(id, self.clock.now().timestamp(), body)
    }
}

/// The Standard Webhooks headers of an inbound delivery.
///
/// # Example
///
/// ```rust
/// use adk_core::webhook::WebhookHeaders;
/// use std::collections::HashMap;
///
/// let headers: HashMap<&str, &str> = [
///     ("webhook-id", "msg_1"),
///     ("webhook-timestamp", "1760000000"),
///     ("webhook-signature", "v1,c2ln"),
/// ]
/// .into();
/// let parsed = WebhookHeaders::from_lookup(|name| headers.get(name).copied())?;
/// assert_eq!(parsed.id, "msg_1");
/// # Ok::<(), adk_core::webhook::WebhookError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookHeaders<'a> {
    /// The `webhook-id` header.
    pub id: &'a str,
    /// The `webhook-timestamp` header, as received.
    pub timestamp: std::borrow::Cow<'a, str>,
    /// The `webhook-signature` header.
    pub signature: &'a str,
}

impl<'a> WebhookHeaders<'a> {
    /// Reads the three headers through `lookup`, which returns a header's value by
    /// lowercase name.
    ///
    /// # Errors
    ///
    /// Returns [`WebhookError::MissingHeader`] when a header is absent, and
    /// [`WebhookError::MalformedHeader`] when the `webhook-id` is empty.
    pub fn from_lookup(
        lookup: impl Fn(&'static str) -> Option<&'a str>,
    ) -> Result<Self, WebhookError> {
        let header = |name| lookup(name).ok_or(WebhookError::MissingHeader(name));
        let id = header(WEBHOOK_ID_HEADER)?;
        if id.trim().is_empty() || id.len() > 256 {
            return Err(WebhookError::MalformedHeader {
                header: WEBHOOK_ID_HEADER,
                reason: "must contain 1 to 256 bytes".to_string(),
            });
        }
        Ok(Self {
            id,
            timestamp: std::borrow::Cow::Borrowed(header(WEBHOOK_TIMESTAMP_HEADER)?),
            signature: header(WEBHOOK_SIGNATURE_HEADER)?,
        })
    }
}

/// A delivery whose signature and timestamp checked out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedWebhook {
    /// The `webhook-id`: identical across retries of one message.
    pub id: String,
    /// When the sender signed this attempt.
    pub timestamp: DateTime<Utc>,
}

/// Verifies inbound Standard Webhooks deliveries.
///
/// Holds one or more secrets: add the next secret with
/// [`with_secret`](Self::with_secret) before the sender rotates, and remove the old one
/// after.
///
/// # Example
///
/// ```rust
/// use adk_core::webhook::{WebhookError, WebhookHeaders, WebhookSecret, WebhookVerifier};
///
/// let verifier = WebhookVerifier::new(WebhookSecret::from_base64("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw")?);
/// let headers = WebhookHeaders { id: "msg_1", timestamp: "0".into(), signature: "v1,AAAA" };
/// // Signed in 1970: far outside the five-minute tolerance.
/// assert!(matches!(verifier.verify(&headers, b"{}"), Err(WebhookError::TimestampExpired { .. })));
/// # Ok::<(), WebhookError>(())
/// ```
#[derive(Debug, Clone)]
pub struct WebhookVerifier {
    secrets: Vec<WebhookSecret>,
    tolerance: Duration,
    clock: Arc<dyn Clock>,
}

impl WebhookVerifier {
    /// Creates a verifier with one secret, the default tolerance, and the system clock.
    pub fn new(secret: WebhookSecret) -> Self {
        Self {
            secrets: vec![secret],
            tolerance: DEFAULT_WEBHOOK_TOLERANCE,
            clock: Arc::new(SystemClock),
        }
    }

    /// Accepts signatures made with `secret` as well.
    pub fn with_secret(mut self, secret: WebhookSecret) -> Self {
        self.secrets.push(secret);
        self
    }

    /// Sets the accepted distance between `webhook-timestamp` and the local clock.
    pub fn with_tolerance(mut self, tolerance: Duration) -> Self {
        self.tolerance = tolerance;
        self
    }

    /// Sets the clock the timestamp is compared against.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The accepted timestamp distance.
    pub fn tolerance(&self) -> Duration {
        self.tolerance
    }

    /// Checks the timestamp, then the signature, of a delivery.
    ///
    /// `body` must be the raw request body: re-serialized JSON changes the bytes and
    /// fails verification.
    ///
    /// # Errors
    ///
    /// Returns a [`WebhookError`] when the timestamp is malformed or outside the
    /// tolerance, or no signature matches a configured secret.
    pub fn verify(
        &self,
        headers: &WebhookHeaders<'_>,
        body: &[u8],
    ) -> Result<VerifiedWebhook, WebhookError> {
        let raw_timestamp = headers.timestamp.trim();
        let timestamp: i64 = raw_timestamp.parse().map_err(|_| WebhookError::MalformedHeader {
            header: WEBHOOK_TIMESTAMP_HEADER,
            reason: "not an integer number of seconds".to_string(),
        })?;
        let now = self.clock.now().timestamp();
        let tolerance = self.tolerance.as_secs();
        let distance = now.abs_diff(timestamp);
        if distance > tolerance {
            return Err(if timestamp < now {
                WebhookError::TimestampExpired { age_seconds: distance, max_age_seconds: tolerance }
            } else {
                WebhookError::TimestampInFuture {
                    skew_seconds: distance,
                    max_skew_seconds: tolerance,
                }
            });
        }

        let candidates: Vec<Vec<u8>> = headers
            .signature
            .split_whitespace()
            .filter_map(|entry| entry.strip_prefix("v1,"))
            .filter_map(|encoded| base64::engine::general_purpose::STANDARD.decode(encoded).ok())
            .collect();
        if candidates.is_empty() {
            return Err(WebhookError::MalformedHeader {
                header: WEBHOOK_SIGNATURE_HEADER,
                reason: "no `v1,<base64>` signature".to_string(),
            });
        }
        let matched = self.secrets.iter().any(|secret| {
            let mac = secret.mac(headers.id, raw_timestamp, body);
            // `verify_slice` compares in constant time.
            candidates.iter().any(|candidate| mac.clone().verify_slice(candidate).is_ok())
        });
        if !matched {
            return Err(WebhookError::SignatureMismatch);
        }
        let timestamp = DateTime::from_timestamp(timestamp, 0).ok_or_else(|| {
            WebhookError::MalformedHeader {
                header: WEBHOOK_TIMESTAMP_HEADER,
                reason: "out of range".to_string(),
            }
        })?;
        Ok(VerifiedWebhook { id: headers.id.to_string(), timestamp })
    }
}

/// Remembers accepted `webhook-id`s so a replay inside the timestamp tolerance is refused.
///
/// A replay outside the tolerance already fails [`WebhookVerifier::verify`], so an id
/// needs to be kept for twice the tolerance only. Use a shared backend when several
/// replicas receive the same endpoint's deliveries.
#[async_trait]
pub trait WebhookReplayGuard: Send + Sync {
    /// Records `id` until `keep_until` and returns `true` when it was not already recorded.
    ///
    /// # Errors
    ///
    /// Returns an error when the backing store cannot be reached; callers refuse the
    /// delivery so the sender retries.
    async fn first_seen(&self, id: &str, keep_until: DateTime<Utc>) -> crate::Result<bool>;
}

/// Process-local [`WebhookReplayGuard`].
///
/// # Example
///
/// ```rust
/// use adk_core::webhook::{InMemoryWebhookReplayGuard, WebhookReplayGuard};
///
/// # futures::executor::block_on(async {
/// let guard = InMemoryWebhookReplayGuard::new();
/// let until = chrono::Utc::now() + chrono::Duration::minutes(10);
/// assert!(guard.first_seen("msg_1", until).await?);
/// assert!(!guard.first_seen("msg_1", until).await?);
/// # Ok::<(), adk_core::AdkError>(())
/// # }).unwrap();
/// ```
#[derive(Debug)]
pub struct InMemoryWebhookReplayGuard {
    seen: std::sync::Mutex<HashMap<String, DateTime<Utc>>>,
    clock: Arc<dyn Clock>,
}

impl Default for InMemoryWebhookReplayGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryWebhookReplayGuard {
    /// Creates an empty guard that reads the system clock.
    pub fn new() -> Self {
        Self { seen: std::sync::Mutex::new(HashMap::new()), clock: Arc::new(SystemClock) }
    }

    /// Sets the clock that decides when a recorded id expires.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Records `id` until `keep_until`; the synchronous form of
    /// [`WebhookReplayGuard::first_seen`].
    pub fn record(&self, id: &str, keep_until: DateTime<Utc>) -> bool {
        let now = self.clock.now();
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        seen.retain(|_, until| *until > now);
        if seen.contains_key(id) {
            return false;
        }
        seen.insert(id.to_string(), keep_until);
        true
    }
}

#[async_trait]
impl WebhookReplayGuard for InMemoryWebhookReplayGuard {
    async fn first_seen(&self, id: &str, keep_until: DateTime<Utc>) -> crate::Result<bool> {
        Ok(self.record(id, keep_until))
    }
}

/// How long a [`WebhookReplayGuard`] must keep an id accepted under `tolerance`.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
///
/// assert_eq!(adk_core::webhook::replay_window(Duration::from_secs(300)), Duration::from_secs(600));
/// ```
pub fn replay_window(tolerance: Duration) -> Duration {
    tolerance.saturating_mul(2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ManualClock;

    const SECRET: &str = "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw";
    const NOW: i64 = 1_760_000_000;

    fn clock() -> Arc<ManualClock> {
        Arc::new(ManualClock::new(DateTime::from_timestamp(NOW, 0).unwrap()))
    }

    fn secret(text: &str) -> WebhookSecret {
        WebhookSecret::from_bytes(text.as_bytes().to_vec()).unwrap()
    }

    #[test]
    fn reference_vector_matches() {
        let secret = WebhookSecret::from_base64(SECRET).unwrap();
        let body = br#"{"test": 2432232314}"#;
        let signed = WebhookSigner::new(secret.clone()).sign(
            "msg_p5jXN8AQM9LWM0D4loKWxJek",
            1_614_265_330,
            body,
        );
        assert_eq!(signed.signature, "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE=");
        let verifier = WebhookVerifier::new(secret).with_clock(Arc::new(ManualClock::new(
            DateTime::from_timestamp(1_614_265_330, 0).unwrap(),
        )));
        assert_eq!(verifier.verify(&signed.as_headers(), body).unwrap().id, signed.id);
    }

    #[test]
    fn valid_signature_is_accepted() {
        let clock = clock();
        let signer = WebhookSigner::new(secret("a")).with_clock(clock.clone());
        let signed = signer.sign_now("msg_1", b"{}");
        let verified =
            WebhookVerifier::new(secret("a")).with_clock(clock).verify(&signed.as_headers(), b"{}");
        assert_eq!(
            verified,
            Ok(VerifiedWebhook {
                id: "msg_1".to_string(),
                timestamp: DateTime::from_timestamp(NOW, 0).unwrap()
            })
        );
    }

    #[test]
    fn tampered_body_wrong_secret_and_wrong_id_are_rejected() {
        let clock = clock();
        let signed = WebhookSigner::new(secret("a")).sign("msg_1", NOW, b"{}");
        let verifier = WebhookVerifier::new(secret("a")).with_clock(clock.clone());
        assert_eq!(
            verifier.verify(&signed.as_headers(), b"{\"x\":1}"),
            Err(WebhookError::SignatureMismatch)
        );
        let other = WebhookVerifier::new(secret("b")).with_clock(clock.clone());
        assert_eq!(other.verify(&signed.as_headers(), b"{}"), Err(WebhookError::SignatureMismatch));
        let mut renamed = signed.clone();
        renamed.id = "msg_other".to_string();
        assert_eq!(
            verifier.verify(&renamed.as_headers(), b"{}"),
            Err(WebhookError::SignatureMismatch)
        );
    }

    #[test]
    fn expired_and_future_timestamps_are_rejected() {
        let clock = clock();
        let verifier = WebhookVerifier::new(secret("a")).with_clock(clock);
        let signer = WebhookSigner::new(secret("a"));
        let old = signer.sign("msg_1", NOW - 301, b"{}");
        assert_eq!(
            verifier.verify(&old.as_headers(), b"{}"),
            Err(WebhookError::TimestampExpired { age_seconds: 301, max_age_seconds: 300 })
        );
        let future = signer.sign("msg_1", NOW + 301, b"{}");
        assert_eq!(
            verifier.verify(&future.as_headers(), b"{}"),
            Err(WebhookError::TimestampInFuture { skew_seconds: 301, max_skew_seconds: 300 })
        );
        let edge = signer.sign("msg_1", NOW - 300, b"{}");
        assert!(verifier.verify(&edge.as_headers(), b"{}").is_ok());
    }

    #[test]
    fn rotation_accepts_either_key_from_either_side() {
        let clock = clock();
        // The sender signs with both keys; a receiver still on the old key accepts it.
        let both =
            WebhookSigner::new(secret("old")).with_secret(secret("new")).sign("m", NOW, b"{}");
        let old_receiver = WebhookVerifier::new(secret("old")).with_clock(clock.clone());
        let new_receiver = WebhookVerifier::new(secret("new")).with_clock(clock.clone());
        assert!(old_receiver.verify(&both.as_headers(), b"{}").is_ok());
        assert!(new_receiver.verify(&both.as_headers(), b"{}").is_ok());
        // The receiver holds both keys; a sender on either key is accepted.
        let receiver =
            WebhookVerifier::new(secret("old")).with_secret(secret("new")).with_clock(clock);
        for key in ["old", "new"] {
            let signed = WebhookSigner::new(secret(key)).sign("m", NOW, b"{}");
            assert!(receiver.verify(&signed.as_headers(), b"{}").is_ok(), "{key}");
        }
        let retired = WebhookSigner::new(secret("retired")).sign("m", NOW, b"{}");
        assert_eq!(
            receiver.verify(&retired.as_headers(), b"{}"),
            Err(WebhookError::SignatureMismatch)
        );
    }

    #[test]
    fn malformed_headers_are_rejected() {
        let verifier = WebhookVerifier::new(secret("a")).with_clock(clock());
        let headers = WebhookHeaders { id: "m", timestamp: "soon".into(), signature: "v1,AAAA" };
        assert!(matches!(
            verifier.verify(&headers, b"{}"),
            Err(WebhookError::MalformedHeader { header: WEBHOOK_TIMESTAMP_HEADER, .. })
        ));
        let headers =
            WebhookHeaders { id: "m", timestamp: NOW.to_string().into(), signature: "v2,AAAA" };
        assert!(matches!(
            verifier.verify(&headers, b"{}"),
            Err(WebhookError::MalformedHeader { header: WEBHOOK_SIGNATURE_HEADER, .. })
        ));
        assert_eq!(
            WebhookHeaders::from_lookup(|_| None),
            Err(WebhookError::MissingHeader(WEBHOOK_ID_HEADER))
        );
        assert!(WebhookSecret::from_base64("whsec_").is_err());
        assert!(WebhookSecret::from_base64("not base64!").is_err());
    }

    #[tokio::test]
    async fn replay_guard_refuses_a_second_delivery_until_the_window_passes() {
        let clock = clock();
        let guard = InMemoryWebhookReplayGuard::new().with_clock(clock.clone());
        let until = clock.now()
            + chrono::Duration::from_std(replay_window(DEFAULT_WEBHOOK_TOLERANCE)).unwrap();
        assert!(guard.first_seen("m", until).await.unwrap());
        assert!(!guard.first_seen("m", until).await.unwrap());
        clock.advance(Duration::from_secs(601));
        assert!(guard.first_seen("m", until).await.unwrap());
    }
}
