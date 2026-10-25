//! The AWP 2.0 error envelope and error codes (spec 3.5 §7.5).

use serde::{Deserialize, Serialize};

use super::contract::VERSION_2_0;
use super::status::SiteStatus;
use crate::AwpVersion;

/// The error envelope `{awp, error: {code, message, retryable, retryAfter, traceId}}`
/// (spec 3.5 §7.5); a `read_only` error also carries the signed status document.
///
/// # Example
///
/// ```
/// use awp_types::{ErrorCode, ErrorEnvelope};
///
/// let envelope = ErrorEnvelope::new(ErrorCode::IdempotencyConflict, "key reused with other arguments");
/// assert_eq!(envelope.error.code.http_status(), 409);
/// let json = serde_json::to_value(&envelope).unwrap();
/// assert_eq!(json["awp"], "2.0");
/// assert_eq!(json["error"]["code"], "idempotency_conflict");
/// assert_eq!(json["error"]["retryable"], false);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ErrorEnvelope {
    /// Protocol version, `"2.0"`.
    #[serde(with = "super::serde_util::version_string")]
    #[cfg_attr(feature = "json-schema", schemars(with = "String"))]
    pub awp: AwpVersion,
    /// The error.
    pub error: ErrorBody,
    /// With `read_only`: the signed status document (spec 3.5 §7.9).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<SiteStatus>,
}

impl ErrorEnvelope {
    /// Creates a 2.0 envelope whose `retryable` is the code's default.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { awp: VERSION_2_0, error: ErrorBody::new(code, message), status: None }
    }

    /// Creates the `read_only` envelope the edge answers non-read capabilities with, carrying
    /// the signed status document (spec 3.5 §7.9).
    pub fn read_only(message: impl Into<String>, status: SiteStatus) -> Self {
        Self {
            awp: VERSION_2_0,
            error: ErrorBody::new(ErrorCode::ReadOnly, message),
            status: Some(status),
        }
    }
}

/// The body of an [`ErrorEnvelope`] (spec 3.5 §7.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ErrorBody {
    /// Error code.
    pub code: ErrorCode,
    /// Human-readable message with no internal detail.
    pub message: String,
    /// Whether the same request can succeed if retried.
    pub retryable: bool,
    /// Seconds to wait before retrying.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<u64>,
    /// Trace id for support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

impl ErrorBody {
    /// Creates a body whose `retryable` is the code's default.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        let retryable = code.is_retryable();
        Self { code, message: message.into(), retryable, retry_after: None, trace_id: None }
    }
}

open_enum! {
    /// An AWP 2.0 error code (spec 3.5 §7.5).
    ///
    /// The AWP 1.0 draft's registry, renamed to snake_case without the `awp:` prefix, plus
    /// the codes the 2.0 edge needs.
    ///
    /// | Code | HTTP | Retryable |
    /// |---|---|---|
    /// | `invalid_request` | 400 | no |
    /// | `schema_validation_failed` | 400 | no |
    /// | `idempotency_key_required` | 400 | no |
    /// | `unauthorized` | 401 | no |
    /// | `payment_policy_rejected` | 402 | no |
    /// | `forbidden` | 403 | no |
    /// | `capability_not_found` | 404 | no |
    /// | `resource_not_found` | 404 | no |
    /// | `version_not_supported` | 406 | no |
    /// | `idempotency_conflict` | 409 | no |
    /// | `inventory_unavailable` | 409 | yes |
    /// | `quote_expired` | 409 | no |
    /// | `subscription_suspended` | 410 | no |
    /// | `rate_limit_exceeded` | 429 | yes |
    /// | `consent_required` | 451 | no |
    /// | `internal_error` | 500 | yes |
    /// | `payment_failed` | 502 | yes |
    /// | `service_degraded` | 503 | yes |
    /// | `read_only` | 503 | yes |
    pub enum ErrorCode {
        /// The request is malformed.
        InvalidRequest => "invalid_request",
        /// The input does not match the capability's input schema.
        SchemaValidationFailed => "schema_validation_failed",
        /// A `NonIdempotent` capability was called without an idempotency key (A-I7).
        IdempotencyKeyRequired => "idempotency_key_required",
        /// The request carries no valid credential.
        Unauthorized => "unauthorized",
        /// The owner's payment policy rejected the payment.
        PaymentPolicyRejected => "payment_policy_rejected",
        /// The caller's verified tier does not allow the capability.
        Forbidden => "forbidden",
        /// No such capability, or not visible to the caller's tier.
        CapabilityNotFound => "capability_not_found",
        /// No such resource.
        ResourceNotFound => "resource_not_found",
        /// No common major version.
        VersionNotSupported => "version_not_supported",
        /// An idempotency key was reused with different arguments.
        IdempotencyConflict => "idempotency_conflict",
        /// Not available in the requested quantity.
        InventoryUnavailable => "inventory_unavailable",
        /// The quote's validity ended (A-I25).
        QuoteExpired => "quote_expired",
        /// The event subscription is suspended.
        SubscriptionSuspended => "subscription_suspended",
        /// The caller exceeded its rate limit.
        RateLimitExceeded => "rate_limit_exceeded",
        /// The action needs a live consent receipt (A-I8).
        ConsentRequired => "consent_required",
        /// The site failed.
        InternalError => "internal_error",
        /// The payment provider failed.
        PaymentFailed => "payment_failed",
        /// The site is degraded.
        ServiceDegraded => "service_degraded",
        /// The site is read-only; only `ReadOnly` capabilities answer.
        ReadOnly => "read_only",
        @other
        /// A code this version does not name.
        Other(String),
    }
}

impl ErrorCode {
    /// Returns the HTTP status the code answers with; unnamed codes answer 500.
    pub fn http_status(&self) -> u16 {
        match self {
            ErrorCode::InvalidRequest
            | ErrorCode::SchemaValidationFailed
            | ErrorCode::IdempotencyKeyRequired => 400,
            ErrorCode::Unauthorized => 401,
            ErrorCode::PaymentPolicyRejected => 402,
            ErrorCode::Forbidden => 403,
            ErrorCode::CapabilityNotFound | ErrorCode::ResourceNotFound => 404,
            ErrorCode::VersionNotSupported => 406,
            ErrorCode::IdempotencyConflict
            | ErrorCode::InventoryUnavailable
            | ErrorCode::QuoteExpired => 409,
            ErrorCode::SubscriptionSuspended => 410,
            ErrorCode::RateLimitExceeded => 429,
            ErrorCode::ConsentRequired => 451,
            ErrorCode::InternalError | ErrorCode::Other(_) => 500,
            ErrorCode::PaymentFailed => 502,
            ErrorCode::ServiceDegraded | ErrorCode::ReadOnly => 503,
        }
    }

    /// Returns whether the same request can succeed if retried, by default.
    pub fn is_retryable(&self) -> bool {
        match self {
            ErrorCode::InventoryUnavailable
            | ErrorCode::RateLimitExceeded
            | ErrorCode::InternalError
            | ErrorCode::PaymentFailed
            | ErrorCode::ServiceDegraded
            | ErrorCode::ReadOnly => true,
            ErrorCode::InvalidRequest
            | ErrorCode::SchemaValidationFailed
            | ErrorCode::IdempotencyKeyRequired
            | ErrorCode::Unauthorized
            | ErrorCode::PaymentPolicyRejected
            | ErrorCode::Forbidden
            | ErrorCode::CapabilityNotFound
            | ErrorCode::ResourceNotFound
            | ErrorCode::VersionNotSupported
            | ErrorCode::IdempotencyConflict
            | ErrorCode::QuoteExpired
            | ErrorCode::SubscriptionSuspended
            | ErrorCode::ConsentRequired
            | ErrorCode::Other(_) => false,
        }
    }
}
