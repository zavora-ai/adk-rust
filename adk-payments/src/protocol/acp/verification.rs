use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use async_trait::async_trait;
use axum::http::HeaderMap;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;

use crate::ACP_STABLE_BASELINE;

/// Clock-skew window [`AcpVerificationConfig::strict`] allows on `Timestamp`.
const STRICT_MAX_TIMESTAMP_SKEW: Duration = Duration::from_secs(300);

/// Replay policy for ACP POST operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdempotencyMode {
    /// Allow POST requests without an `Idempotency-Key`.
    Optional,
    /// Reject any ACP POST request that omits `Idempotency-Key`.
    RequireForPost,
}

/// Cached ACP response used for deterministic idempotent replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredIdempotentResponse {
    pub status: u16,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub body: Vec<u8>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
}

/// Idempotency store decision for one ACP POST request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdempotencyDecision {
    Proceed,
    Replay(StoredIdempotentResponse),
    Conflict,
    InFlight,
}

/// Pluggable ACP idempotency storage.
#[async_trait]
pub trait IdempotencyStore: Send + Sync {
    /// Starts an ACP idempotency attempt for one scoped request.
    async fn begin_request(
        &self,
        scope: &str,
        idempotency_key: &str,
        fingerprint: &str,
    ) -> Result<IdempotencyDecision>;

    /// Stores a successful or client-error response for replay.
    async fn finish_request(
        &self,
        scope: &str,
        idempotency_key: &str,
        fingerprint: &str,
        response: StoredIdempotentResponse,
    ) -> Result<()>;

    /// Releases an in-flight reservation after a non-cacheable failure.
    async fn abort_request(
        &self,
        scope: &str,
        idempotency_key: &str,
        fingerprint: &str,
    ) -> Result<()>;
}

/// In-memory idempotency storage for ACP development and tests.
#[derive(Default)]
pub struct InMemoryIdempotencyStore {
    entries: RwLock<BTreeMap<String, InMemoryEntry>>,
}

/// Pluggable detached signature verifier for ACP requests.
#[async_trait]
pub trait DetachedSignatureVerifier: Send + Sync {
    /// Verifies the detached request signature against the canonical request.
    async fn verify(
        &self,
        signature: &str,
        timestamp: Option<DateTime<Utc>>,
        method: &str,
        path: &str,
        body: &[u8],
    ) -> Result<()>;
}

/// ACP request verification settings.
#[derive(Clone)]
pub struct AcpVerificationConfig {
    pub(crate) supported_api_versions: Vec<String>,
    pub(crate) idempotency_mode: IdempotencyMode,
    pub(crate) require_timestamp: bool,
    pub(crate) max_timestamp_skew: Option<Duration>,
    pub(crate) require_signature: bool,
    pub(crate) signature_verifier: Option<Arc<dyn DetachedSignatureVerifier>>,
    pub(crate) idempotency_store: Arc<dyn IdempotencyStore>,
}

#[derive(Clone)]
pub(crate) struct AcpRequestVerifier {
    config: AcpVerificationConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifiedRequestHeaders {
    pub(crate) api_version: String,
    pub(crate) request_id: Option<String>,
    pub(crate) idempotency_key: Option<String>,
    pub(crate) timestamp: Option<DateTime<Utc>>,
    pub(crate) signature_present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifiedRequest {
    pub(crate) headers: VerifiedRequestHeaders,
    pub(crate) replay: Option<StoredIdempotentResponse>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum AcpVerificationError {
    #[error("API-Version header is required")]
    MissingApiVersion,

    #[error("unsupported API-Version `{version}`")]
    UnsupportedApiVersion { version: String, supported: Vec<String> },

    #[error("Timestamp header is required")]
    MissingTimestamp,

    #[error("invalid Timestamp header value `{value}`")]
    InvalidTimestamp { value: String },

    #[error("Timestamp header is outside the allowed clock skew window")]
    TimestampSkew,

    #[error("Signature header is required")]
    MissingSignature,

    #[error("detached request signature verification failed")]
    InvalidSignature,

    #[error(
        "Signature is required but no signature verifier is configured. Add one with `AcpVerificationConfig::with_signature_verifier`."
    )]
    SignatureVerifierNotConfigured,

    #[error("Idempotency-Key header is required on all POST requests")]
    MissingIdempotencyKey,

    #[error("Idempotency-Key has already been used with a different request body")]
    IdempotencyConflict,

    #[error("A request with this Idempotency-Key is currently being processed")]
    IdempotencyInFlight,

    #[error(transparent)]
    Internal(#[from] AdkError),
}

impl InMemoryIdempotencyStore {
    /// Creates an in-memory ACP idempotency store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn entry_key(scope: &str, idempotency_key: &str) -> String {
        format!("{scope}:{idempotency_key}")
    }
}

#[async_trait]
impl IdempotencyStore for InMemoryIdempotencyStore {
    async fn begin_request(
        &self,
        scope: &str,
        idempotency_key: &str,
        fingerprint: &str,
    ) -> Result<IdempotencyDecision> {
        let key = Self::entry_key(scope, idempotency_key);
        let mut entries = self.entries.write().await;

        match entries.get(&key) {
            Some(InMemoryEntry::InFlight { fingerprint: stored }) if stored == fingerprint => {
                Ok(IdempotencyDecision::InFlight)
            }
            Some(InMemoryEntry::InFlight { .. }) => Ok(IdempotencyDecision::Conflict),
            Some(InMemoryEntry::Complete { fingerprint: stored, response })
                if stored == fingerprint =>
            {
                Ok(IdempotencyDecision::Replay(response.clone()))
            }
            Some(InMemoryEntry::Complete { .. }) => Ok(IdempotencyDecision::Conflict),
            None => {
                entries
                    .insert(key, InMemoryEntry::InFlight { fingerprint: fingerprint.to_string() });
                Ok(IdempotencyDecision::Proceed)
            }
        }
    }

    async fn finish_request(
        &self,
        scope: &str,
        idempotency_key: &str,
        fingerprint: &str,
        response: StoredIdempotentResponse,
    ) -> Result<()> {
        let key = Self::entry_key(scope, idempotency_key);
        let mut entries = self.entries.write().await;
        entries.insert(
            key,
            InMemoryEntry::Complete { fingerprint: fingerprint.to_string(), response },
        );
        Ok(())
    }

    async fn abort_request(
        &self,
        scope: &str,
        idempotency_key: &str,
        fingerprint: &str,
    ) -> Result<()> {
        let key = Self::entry_key(scope, idempotency_key);
        let mut entries = self.entries.write().await;
        let should_remove = matches!(
            entries.get(&key),
            Some(InMemoryEntry::InFlight { fingerprint: stored }) if stored == fingerprint
        );
        if should_remove {
            entries.remove(&key);
        }
        Ok(())
    }
}

impl Default for AcpVerificationConfig {
    fn default() -> Self {
        Self {
            supported_api_versions: vec![ACP_STABLE_BASELINE.to_string()],
            idempotency_mode: IdempotencyMode::Optional,
            require_timestamp: false,
            max_timestamp_skew: None,
            require_signature: false,
            signature_verifier: None,
            idempotency_store: Arc::new(InMemoryIdempotencyStore::new()),
        }
    }
}

impl AcpVerificationConfig {
    /// Creates a permissive ACP verification profile.
    ///
    /// Only `API-Version` is enforced. Signatures are verified when both a
    /// `Signature` header and a verifier are present, but unsigned requests
    /// pass. Suitable for development and tests.
    #[must_use]
    pub fn permissive() -> Self {
        Self::default()
    }

    /// Creates a strict ACP verification profile for production use.
    ///
    /// Every request must carry a `Timestamp` within five minutes of the
    /// server clock and a `Signature` that the configured
    /// [`DetachedSignatureVerifier`] accepts, and every POST must carry an
    /// `Idempotency-Key`. Add the verifier with
    /// [`with_signature_verifier`](Self::with_signature_verifier); building a
    /// router from a strict profile without one fails.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_payments::protocol::acp::AcpVerificationConfig;
    ///
    /// let verification = AcpVerificationConfig::strict().with_signature_verifier(verifier);
    /// ```
    #[must_use]
    pub fn strict() -> Self {
        Self::default()
            .with_idempotency_mode(IdempotencyMode::RequireForPost)
            .with_max_timestamp_skew(STRICT_MAX_TIMESTAMP_SKEW)
            .require_signature(true)
    }

    /// Replaces the supported `API-Version` set.
    #[must_use]
    pub fn with_supported_api_versions(mut self, versions: Vec<String>) -> Self {
        self.supported_api_versions = versions;
        self
    }

    /// Sets the ACP POST idempotency policy.
    #[must_use]
    pub fn with_idempotency_mode(mut self, mode: IdempotencyMode) -> Self {
        self.idempotency_mode = mode;
        self
    }

    /// Requires `Timestamp` and enforces a maximum clock skew.
    #[must_use]
    pub fn with_max_timestamp_skew(mut self, max_timestamp_skew: Duration) -> Self {
        self.require_timestamp = true;
        self.max_timestamp_skew = Some(max_timestamp_skew);
        self
    }

    /// Forces `Timestamp` even when skew checking is disabled.
    #[must_use]
    pub fn require_timestamp(mut self, require_timestamp: bool) -> Self {
        self.require_timestamp = require_timestamp;
        self
    }

    /// Adds a detached signature verifier.
    ///
    /// A present `Signature` header is always verified. Unsigned requests
    /// still pass unless [`require_signature`](Self::require_signature) is
    /// enabled.
    #[must_use]
    pub fn with_signature_verifier(mut self, verifier: Arc<dyn DetachedSignatureVerifier>) -> Self {
        self.signature_verifier = Some(verifier);
        self
    }

    /// Requires a verified `Signature` header on every request.
    ///
    /// Requires a verifier from
    /// [`with_signature_verifier`](Self::with_signature_verifier): building a
    /// router with `require_signature(true)` and no verifier fails, and such a
    /// profile rejects every request.
    #[must_use]
    pub fn require_signature(mut self, require_signature: bool) -> Self {
        self.require_signature = require_signature;
        self
    }

    /// Replaces the ACP idempotency store implementation.
    #[must_use]
    pub fn with_idempotency_store(mut self, store: Arc<dyn IdempotencyStore>) -> Self {
        self.idempotency_store = store;
        self
    }

    /// Rejects profiles that would claim signature enforcement without a verifier.
    pub(crate) fn validate(&self) -> Result<()> {
        if self.require_signature && self.signature_verifier.is_none() {
            return Err(AcpVerificationError::SignatureVerifierNotConfigured.into());
        }
        Ok(())
    }
}

impl AcpRequestVerifier {
    pub(crate) fn new(config: AcpVerificationConfig) -> Self {
        Self { config }
    }

    pub(crate) async fn verify(
        &self,
        method: &str,
        path: &str,
        headers: &HeaderMap,
        body: &[u8],
    ) -> std::result::Result<VerifiedRequest, AcpVerificationError> {
        let api_version =
            header_value(headers, "API-Version").ok_or(AcpVerificationError::MissingApiVersion)?;
        if !self.config.supported_api_versions.iter().any(|supported| supported == &api_version) {
            return Err(AcpVerificationError::UnsupportedApiVersion {
                version: api_version.clone(),
                supported: self.config.supported_api_versions.clone(),
            });
        }

        let timestamp = match header_value(headers, "Timestamp") {
            Some(value) => Some(
                DateTime::parse_from_rfc3339(&value)
                    .map_err(|_| AcpVerificationError::InvalidTimestamp { value: value.clone() })?
                    .with_timezone(&Utc),
            ),
            None if self.config.require_timestamp || self.config.max_timestamp_skew.is_some() => {
                return Err(AcpVerificationError::MissingTimestamp);
            }
            None => None,
        };

        if let (Some(timestamp), Some(max_skew)) = (timestamp, self.config.max_timestamp_skew) {
            let skew = (Utc::now() - timestamp)
                .to_std()
                .or_else(|_| (timestamp - Utc::now()).to_std())
                .unwrap_or_default();
            if skew > max_skew {
                return Err(AcpVerificationError::TimestampSkew);
            }
        }

        let signature = header_value(headers, "Signature");
        match (&self.config.signature_verifier, &signature) {
            (Some(verifier), Some(signature)) => {
                verifier
                    .verify(signature, timestamp, method, path, body)
                    .await
                    .map_err(|_| AcpVerificationError::InvalidSignature)?;
            }
            (None, _) if self.config.require_signature => {
                return Err(AcpVerificationError::SignatureVerifierNotConfigured);
            }
            (_, None) if self.config.require_signature => {
                return Err(AcpVerificationError::MissingSignature);
            }
            (Some(_), None) | (None, _) => {}
        }

        let idempotency_key = header_value(headers, "Idempotency-Key");
        if method.eq_ignore_ascii_case("POST")
            && self.config.idempotency_mode == IdempotencyMode::RequireForPost
            && idempotency_key.is_none()
        {
            return Err(AcpVerificationError::MissingIdempotencyKey);
        }

        let replay = if method.eq_ignore_ascii_case("POST") {
            self.prepare_idempotency(method, path, idempotency_key.as_deref(), body).await?
        } else {
            None
        };

        Ok(VerifiedRequest {
            headers: VerifiedRequestHeaders {
                api_version,
                request_id: header_value(headers, "Request-Id"),
                idempotency_key,
                timestamp,
                signature_present: signature.is_some(),
            },
            replay,
        })
    }

    pub(crate) async fn finalize(
        &self,
        method: &str,
        path: &str,
        idempotency_key: Option<&str>,
        body: &[u8],
        response: &StoredIdempotentResponse,
    ) -> std::result::Result<(), AcpVerificationError> {
        if !method.eq_ignore_ascii_case("POST") {
            return Ok(());
        }

        let Some(idempotency_key) = idempotency_key else {
            return Ok(());
        };
        let fingerprint = fingerprint(method, path, body);
        if response.status >= 500 {
            self.config
                .idempotency_store
                .abort_request(path, idempotency_key, &fingerprint)
                .await?;
        } else {
            self.config
                .idempotency_store
                .finish_request(path, idempotency_key, &fingerprint, response.clone())
                .await?;
        }
        Ok(())
    }

    async fn prepare_idempotency(
        &self,
        method: &str,
        path: &str,
        idempotency_key: Option<&str>,
        body: &[u8],
    ) -> std::result::Result<Option<StoredIdempotentResponse>, AcpVerificationError> {
        let Some(idempotency_key) = idempotency_key else {
            return Ok(None);
        };
        let fingerprint = fingerprint(method, path, body);
        match self
            .config
            .idempotency_store
            .begin_request(path, idempotency_key, &fingerprint)
            .await?
        {
            IdempotencyDecision::Proceed => Ok(None),
            IdempotencyDecision::Replay(response) => Ok(Some(response)),
            IdempotencyDecision::Conflict => Err(AcpVerificationError::IdempotencyConflict),
            IdempotencyDecision::InFlight => Err(AcpVerificationError::IdempotencyInFlight),
        }
    }
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(name).and_then(|value| value.to_str().ok()).map(str::to_string)
}

fn fingerprint(method: &str, path: &str, body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(method.as_bytes());
    hasher.update(b":");
    hasher.update(path.as_bytes());
    hasher.update(b":");
    hasher.update(body);
    hex::encode(hasher.finalize())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InMemoryEntry {
    InFlight { fingerprint: String },
    Complete { fingerprint: String, response: StoredIdempotentResponse },
}

impl AcpVerificationError {
    pub(crate) fn status_code(&self) -> u16 {
        match self {
            Self::MissingApiVersion
            | Self::UnsupportedApiVersion { .. }
            | Self::MissingTimestamp
            | Self::InvalidTimestamp { .. }
            | Self::TimestampSkew
            | Self::MissingSignature
            | Self::InvalidSignature
            | Self::MissingIdempotencyKey => 400,
            Self::IdempotencyConflict => 409,
            Self::IdempotencyInFlight => 409,
            Self::SignatureVerifierNotConfigured => 500,
            Self::Internal(error) => error.http_status_code(),
        }
    }

    pub(crate) fn response_type(&self) -> &'static str {
        match self {
            Self::Internal(error) if error.category == ErrorCategory::RateLimited => {
                "rate_limit_exceeded"
            }
            Self::Internal(error)
                if matches!(
                    error.category,
                    ErrorCategory::Unavailable | ErrorCategory::Timeout
                ) =>
            {
                "service_unavailable"
            }
            Self::SignatureVerifierNotConfigured => "processing_error",
            _ => "invalid_request",
        }
    }

    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::MissingApiVersion => "api_version_required",
            Self::UnsupportedApiVersion { .. } => "unsupported_api_version",
            Self::MissingTimestamp => "timestamp_required",
            Self::InvalidTimestamp { .. } => "invalid_timestamp",
            Self::TimestampSkew => "timestamp_out_of_range",
            Self::MissingSignature => "signature_required",
            Self::InvalidSignature => "invalid_signature",
            Self::SignatureVerifierNotConfigured => "signature_verifier_not_configured",
            Self::MissingIdempotencyKey => "idempotency_key_required",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::IdempotencyInFlight => "idempotency_in_flight",
            Self::Internal(error) => match error.category {
                ErrorCategory::RateLimited => "too_many_requests",
                ErrorCategory::Unavailable | ErrorCategory::Timeout => "service_unavailable",
                ErrorCategory::Unauthorized => "unauthorized",
                ErrorCategory::NotFound => "not_found",
                _ => "processing_error",
            },
        }
    }
}

impl From<AcpVerificationError> for AdkError {
    fn from(value: AcpVerificationError) -> Self {
        match value {
            AcpVerificationError::MissingApiVersion => AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::InvalidInput,
                "payments.acp.api_version_required",
                "API-Version header is required",
            ),
            AcpVerificationError::UnsupportedApiVersion { version, supported } => AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::Unsupported,
                "payments.acp.unsupported_api_version",
                format!(
                    "unsupported ACP API-Version `{version}`. Supported versions: {}",
                    supported.join(", ")
                ),
            ),
            AcpVerificationError::MissingTimestamp => AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::InvalidInput,
                "payments.acp.timestamp_required",
                "Timestamp header is required",
            ),
            AcpVerificationError::InvalidTimestamp { value } => AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::InvalidInput,
                "payments.acp.invalid_timestamp",
                format!("invalid Timestamp header value `{value}`"),
            ),
            AcpVerificationError::TimestampSkew => AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::InvalidInput,
                "payments.acp.timestamp_out_of_range",
                "Timestamp header is outside the allowed clock skew window",
            ),
            AcpVerificationError::MissingSignature => AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::InvalidInput,
                "payments.acp.signature_required",
                "Signature header is required",
            ),
            AcpVerificationError::InvalidSignature => AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::Forbidden,
                "payments.acp.invalid_signature",
                "detached request signature verification failed",
            ),
            AcpVerificationError::SignatureVerifierNotConfigured => AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::Internal,
                "payments.acp.signature_verifier_not_configured",
                "ACP signature enforcement is enabled but no signature verifier is configured. Add one with `AcpVerificationConfig::with_signature_verifier`, or disable `require_signature`.",
            ),
            AcpVerificationError::MissingIdempotencyKey => AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::InvalidInput,
                "payments.acp.idempotency_required",
                "Idempotency-Key header is required on all POST requests",
            ),
            AcpVerificationError::IdempotencyConflict => AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::InvalidInput,
                "payments.acp.idempotency_conflict",
                "Idempotency-Key has already been used with a different request body",
            ),
            AcpVerificationError::IdempotencyInFlight => AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::InvalidInput,
                "payments.acp.idempotency_in_flight",
                "A request with this Idempotency-Key is currently being processed",
            ),
            AcpVerificationError::Internal(error) => error,
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};

    use super::*;

    /// Accepts or rejects every signature without inspecting it.
    struct FixedVerdictVerifier {
        accept: bool,
    }

    #[async_trait]
    impl DetachedSignatureVerifier for FixedVerdictVerifier {
        async fn verify(
            &self,
            _signature: &str,
            _timestamp: Option<DateTime<Utc>>,
            _method: &str,
            _path: &str,
            _body: &[u8],
        ) -> Result<()> {
            if self.accept {
                Ok(())
            } else {
                Err(AdkError::new(
                    ErrorComponent::Server,
                    ErrorCategory::Forbidden,
                    "payments.acp.test.signature_rejected",
                    "test verifier rejects every signature",
                ))
            }
        }
    }

    fn strict_with_verdict(accept: bool) -> AcpRequestVerifier {
        AcpRequestVerifier::new(
            AcpVerificationConfig::strict()
                .with_signature_verifier(Arc::new(FixedVerdictVerifier { accept })),
        )
    }

    fn signed_headers(timestamp: DateTime<Utc>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("API-Version", HeaderValue::from_static(ACP_STABLE_BASELINE));
        headers.insert("Timestamp", HeaderValue::from_str(&timestamp.to_rfc3339()).unwrap());
        headers.insert("Signature", HeaderValue::from_static("sig"));
        headers
    }

    #[test]
    fn strict_profile_without_signature_verifier_fails_validation() {
        let error = AcpVerificationConfig::strict().validate().unwrap_err();
        assert_eq!(error.code, "payments.acp.signature_verifier_not_configured");

        let error =
            AcpVerificationConfig::permissive().require_signature(true).validate().unwrap_err();
        assert_eq!(error.code, "payments.acp.signature_verifier_not_configured");

        AcpVerificationConfig::permissive().validate().unwrap();
        AcpVerificationConfig::strict()
            .with_signature_verifier(Arc::new(FixedVerdictVerifier { accept: true }))
            .validate()
            .unwrap();
    }

    #[tokio::test]
    async fn required_signature_without_verifier_rejects_even_signed_requests() {
        let verifier =
            AcpRequestVerifier::new(AcpVerificationConfig::permissive().require_signature(true));

        let error = verifier
            .verify("GET", "/checkout_sessions/cs_1", &signed_headers(Utc::now()), b"")
            .await
            .unwrap_err();

        assert!(matches!(error, AcpVerificationError::SignatureVerifierNotConfigured));
        assert_eq!(error.status_code(), 500);
    }

    #[tokio::test]
    async fn strict_profile_requires_timestamp_and_signature() {
        let verifier = strict_with_verdict(true);

        let mut headers = signed_headers(Utc::now());
        headers.remove("Timestamp");
        let error = verifier.verify("GET", "/checkout_sessions/cs_1", &headers, b"").await;
        assert!(matches!(error, Err(AcpVerificationError::MissingTimestamp)));

        let mut headers = signed_headers(Utc::now());
        headers.remove("Signature");
        let error = verifier.verify("GET", "/checkout_sessions/cs_1", &headers, b"").await;
        assert!(matches!(error, Err(AcpVerificationError::MissingSignature)));

        let stale = signed_headers(Utc::now() - chrono::Duration::minutes(10));
        let error = verifier.verify("GET", "/checkout_sessions/cs_1", &stale, b"").await;
        assert!(matches!(error, Err(AcpVerificationError::TimestampSkew)));

        let verified = verifier
            .verify("GET", "/checkout_sessions/cs_1", &signed_headers(Utc::now()), b"")
            .await
            .unwrap();
        assert!(verified.headers.signature_present);
    }

    #[tokio::test]
    async fn strict_profile_rejects_signature_the_verifier_refuses() {
        let error = strict_with_verdict(false)
            .verify("GET", "/checkout_sessions/cs_1", &signed_headers(Utc::now()), b"")
            .await
            .unwrap_err();

        assert!(matches!(error, AcpVerificationError::InvalidSignature));
    }

    #[tokio::test]
    async fn strict_mode_rejects_post_without_idempotency_key() {
        let error = strict_with_verdict(true)
            .verify(
                "POST",
                "/checkout_sessions",
                &signed_headers(Utc::now()),
                br#"{"currency":"usd"}"#,
            )
            .await
            .unwrap_err();

        assert!(matches!(error, AcpVerificationError::MissingIdempotencyKey));
    }

    #[tokio::test]
    async fn replays_identical_request_and_rejects_conflicting_payload() {
        let verifier = AcpRequestVerifier::new(
            AcpVerificationConfig::permissive()
                .with_idempotency_mode(IdempotencyMode::RequireForPost)
                .with_idempotency_store(Arc::new(InMemoryIdempotencyStore::new())),
        );
        let mut headers = HeaderMap::new();
        headers.insert("API-Version", HeaderValue::from_static(ACP_STABLE_BASELINE));
        headers.insert("Idempotency-Key", HeaderValue::from_static("idem-123"));

        let first = verifier
            .verify("POST", "/checkout_sessions", &headers, br#"{"currency":"usd"}"#)
            .await
            .unwrap();
        assert_eq!(first.replay, None);

        verifier
            .finalize(
                "POST",
                "/checkout_sessions",
                first.headers.idempotency_key.as_deref(),
                br#"{"currency":"usd"}"#,
                &StoredIdempotentResponse {
                    status: 201,
                    body: br#"{"id":"checkout_session_123"}"#.to_vec(),
                    headers: BTreeMap::new(),
                },
            )
            .await
            .unwrap();

        let replay = verifier
            .verify("POST", "/checkout_sessions", &headers, br#"{"currency":"usd"}"#)
            .await
            .unwrap();
        assert_eq!(replay.replay.unwrap().body, br#"{"id":"checkout_session_123"}"#.to_vec());

        let conflict = verifier
            .verify("POST", "/checkout_sessions", &headers, br#"{"currency":"eur"}"#)
            .await
            .unwrap_err();
        assert!(matches!(conflict, AcpVerificationError::IdempotencyConflict));
    }

    #[tokio::test]
    async fn enforces_timestamp_skew_when_configured() {
        let verifier = AcpRequestVerifier::new(
            AcpVerificationConfig::permissive().with_max_timestamp_skew(Duration::from_secs(5)),
        );
        let mut headers = HeaderMap::new();
        headers.insert("API-Version", HeaderValue::from_static(ACP_STABLE_BASELINE));
        headers.insert(
            "Timestamp",
            HeaderValue::from_str(&(Utc::now() - chrono::Duration::minutes(5)).to_rfc3339())
                .unwrap(),
        );

        let error = verifier
            .verify("GET", "/checkout_sessions/checkout_session_123", &headers, b"")
            .await
            .unwrap_err();
        assert!(matches!(error, AcpVerificationError::TimestampSkew));
    }
}
