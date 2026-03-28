use std::time::Duration;

use axum::body::Bytes;
use axum::http::HeaderMap;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Verification settings for signed ACP merchant webhooks.
#[derive(Clone)]
pub struct AcpMerchantWebhookVerificationConfig {
    pub(crate) secret: Vec<u8>,
    pub(crate) tolerance: Duration,
}

#[derive(Clone)]
pub(crate) struct AcpMerchantWebhookVerifier {
    config: AcpMerchantWebhookVerificationConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifiedMerchantWebhookHeaders {
    pub(crate) request_id: Option<String>,
    pub(crate) signed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct AcpWebhookEvent {
    pub(crate) r#type: AcpWebhookEventType,
    pub(crate) data: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum AcpWebhookEventType {
    #[serde(rename = "order_create")]
    OrderCreate,
    #[serde(rename = "order_update")]
    OrderUpdate,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum MerchantWebhookVerificationError {
    #[error("Missing Merchant-Signature header.")]
    MissingSignature,

    #[error("Merchant-Signature must be t=<timestamp>,v1=<64_hex>.")]
    MalformedSignature,

    #[error("Timestamp outside allowed window.")]
    TimestampExpired,

    #[error("Webhook signature verification failed.")]
    InvalidSignature,
}

impl AcpMerchantWebhookVerificationConfig {
    /// Creates merchant-webhook verification settings from the shared secret.
    #[must_use]
    pub fn new(secret: impl Into<Vec<u8>>) -> Self {
        Self { secret: secret.into(), tolerance: Duration::from_secs(300) }
    }

    /// Overrides the timestamp tolerance used for replay protection.
    #[must_use]
    pub fn with_tolerance(mut self, tolerance: Duration) -> Self {
        self.tolerance = tolerance;
        self
    }
}

impl AcpMerchantWebhookVerifier {
    pub(crate) fn new(config: AcpMerchantWebhookVerificationConfig) -> Self {
        Self { config }
    }

    pub(crate) fn verify(
        &self,
        headers: &HeaderMap,
        body: &Bytes,
    ) -> Result<VerifiedMerchantWebhookHeaders, MerchantWebhookVerificationError> {
        let signature = headers
            .get("Merchant-Signature")
            .and_then(|value| value.to_str().ok())
            .ok_or(MerchantWebhookVerificationError::MissingSignature)?;
        let (timestamp, signature) = parse_signature(signature)?;
        let now = Utc::now().timestamp();
        if now.abs_diff(timestamp) > self.config.tolerance.as_secs() {
            return Err(MerchantWebhookVerificationError::TimestampExpired);
        }

        let mut mac = HmacSha256::new_from_slice(&self.config.secret)
            .expect("HMAC-SHA256 accepts arbitrary key lengths");
        mac.update(timestamp.to_string().as_bytes());
        mac.update(b".");
        mac.update(body);
        mac.verify_slice(&signature)
            .map_err(|_| MerchantWebhookVerificationError::InvalidSignature)?;

        let signed_at = DateTime::from_timestamp(timestamp, 0)
            .ok_or(MerchantWebhookVerificationError::MalformedSignature)?;
        let request_id =
            headers.get("Request-Id").and_then(|value| value.to_str().ok()).map(ToOwned::to_owned);

        Ok(VerifiedMerchantWebhookHeaders { request_id, signed_at })
    }
}

impl AcpWebhookEventType {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::OrderCreate => "order_create",
            Self::OrderUpdate => "order_update",
        }
    }
}

fn parse_signature(value: &str) -> Result<(i64, Vec<u8>), MerchantWebhookVerificationError> {
    let mut timestamp = None;
    let mut signature = None;

    for part in value.split(',') {
        let (key, raw_value) =
            part.split_once('=').ok_or(MerchantWebhookVerificationError::MalformedSignature)?;
        match key.trim() {
            "t" => {
                timestamp = Some(
                    raw_value
                        .trim()
                        .parse::<i64>()
                        .map_err(|_| MerchantWebhookVerificationError::MalformedSignature)?,
                );
            }
            "v1" => {
                let decoded = hex::decode(raw_value.trim())
                    .map_err(|_| MerchantWebhookVerificationError::MalformedSignature)?;
                if decoded.len() != 32 {
                    return Err(MerchantWebhookVerificationError::MalformedSignature);
                }
                signature = Some(decoded);
            }
            _ => {}
        }
    }

    match (timestamp, signature) {
        (Some(timestamp), Some(signature)) => Ok((timestamp, signature)),
        _ => Err(MerchantWebhookVerificationError::MalformedSignature),
    }
}
