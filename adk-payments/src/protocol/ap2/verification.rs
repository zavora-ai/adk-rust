use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use adk_core::Result;
use async_trait::async_trait;

use crate::protocol::ap2::error::Ap2Error;
use crate::protocol::ap2::types::{
    AuthorizationArtifact, CartMandate, IntentMandate, PaymentMandate,
};

/// Claim a [`MerchantAuthorizationVerifier`] sets to the merchant name bound to
/// the verified signing key.
///
/// When present, `Ap2Adapter` rejects a cart whose self-reported
/// `contents.merchant_name` differs from this value, so intent-mandate merchant
/// allow-lists are enforced against the verified identity.
pub const VERIFIED_MERCHANT_NAME_CLAIM: &str = "verified_merchant_name";

/// Claim recording whether an authorization artifact was cryptographically verified.
///
/// The presence-only development verifiers set it to `false`.
pub const CRYPTOGRAPHICALLY_VERIFIED_CLAIM: &str = "cryptographically_verified";

/// Verification metadata captured after one authorization artifact passes policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedAuthorization {
    pub artifact_kind: String,
    pub verified_at: chrono::DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub claims: Map<String, Value>,
}

impl VerifiedAuthorization {
    /// Creates verification metadata stamped with the current time.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_payments::protocol::ap2::{VERIFIED_MERCHANT_NAME_CLAIM, VerifiedAuthorization};
    /// use serde_json::{Map, json};
    ///
    /// let mut claims = Map::new();
    /// claims.insert(VERIFIED_MERCHANT_NAME_CLAIM.to_string(), json!("Merchant Example"));
    /// let verified = VerifiedAuthorization::new("merchant_authorization", claims);
    /// assert_eq!(verified.artifact_kind, "merchant_authorization");
    /// ```
    #[must_use]
    pub fn new(artifact_kind: impl Into<String>, claims: Map<String, Value>) -> Self {
        Self { artifact_kind: artifact_kind.into(), verified_at: Utc::now(), claims }
    }
}

/// Verifies merchant authorization artifacts bound to `CartMandate`.
///
/// Implementations must verify the artifact cryptographically against the
/// cart contents (for example a JWS over the canonical `contents` payload),
/// bind the signing key to the merchant named in `contents.merchant_name`, and
/// compare signatures or digests in constant time. Set
/// [`VERIFIED_MERCHANT_NAME_CLAIM`] to the merchant name the key is bound to.
#[async_trait]
pub trait MerchantAuthorizationVerifier: Send + Sync {
    /// Verifies `artifact` for `mandate` and returns the verified claims.
    ///
    /// # Errors
    ///
    /// Returns an error when the artifact is missing, malformed, expired, or
    /// does not verify against the mandate contents.
    async fn verify_cart_authorization(
        &self,
        mandate: &CartMandate,
        artifact: &AuthorizationArtifact,
    ) -> Result<VerifiedAuthorization>;
}

/// Verifies user authorization artifacts bound to intent or payment mandates.
///
/// Implementations must verify the artifact cryptographically against the
/// mandate contents, check the user's key or credential binding, and compare
/// signatures or digests in constant time.
#[async_trait]
pub trait UserAuthorizationVerifier: Send + Sync {
    /// Verifies `artifact` for an intent mandate and returns the verified claims.
    ///
    /// # Errors
    ///
    /// Returns an error when the artifact does not verify against the intent.
    async fn verify_intent_authorization(
        &self,
        mandate: &IntentMandate,
        artifact: &AuthorizationArtifact,
    ) -> Result<VerifiedAuthorization>;

    /// Verifies `artifact` for a payment mandate and returns the verified claims.
    ///
    /// # Errors
    ///
    /// Returns an error when the artifact does not verify against the payment
    /// mandate contents.
    async fn verify_payment_authorization(
        &self,
        mandate: &PaymentMandate,
        artifact: &AuthorizationArtifact,
    ) -> Result<VerifiedAuthorization>;
}

/// Presence-only merchant check that performs **no cryptographic verification**.
///
/// Any non-empty value passes, including a forged one. `Ap2Adapter` uses it
/// only after `allow_unverified_authorizations()` is called, for local
/// development and tests. Never configure it in production.
pub struct RequireMerchantAuthorization;

#[async_trait]
impl MerchantAuthorizationVerifier for RequireMerchantAuthorization {
    async fn verify_cart_authorization(
        &self,
        mandate: &CartMandate,
        artifact: &AuthorizationArtifact,
    ) -> Result<VerifiedAuthorization> {
        if artifact.value.trim().is_empty() {
            return Err(Ap2Error::MissingMerchantAuthorization {
                cart_id: mandate.contents.id.clone(),
            }
            .into());
        }

        let mut claims = Map::new();
        claims.insert("merchant_name".to_string(), json!(mandate.contents.merchant_name));
        claims.insert("artifact_type".to_string(), json!(artifact.artifact_type));
        claims.insert("content_type".to_string(), json!(artifact.content_type));
        claims.insert(CRYPTOGRAPHICALLY_VERIFIED_CLAIM.to_string(), json!(false));
        Ok(VerifiedAuthorization::new("merchant_authorization", claims))
    }
}

/// Presence-only user check that performs **no cryptographic verification**.
///
/// Any non-empty value passes, including a forged one. `Ap2Adapter` uses it
/// only after `allow_unverified_authorizations()` is called, for local
/// development and tests. Never configure it in production.
pub struct RequireUserAuthorization;

#[async_trait]
impl UserAuthorizationVerifier for RequireUserAuthorization {
    async fn verify_intent_authorization(
        &self,
        mandate: &IntentMandate,
        artifact: &AuthorizationArtifact,
    ) -> Result<VerifiedAuthorization> {
        if artifact.value.trim().is_empty() {
            return Err(Ap2Error::MissingIntentAuthorization {
                transaction_id: mandate.natural_language_description.clone(),
            }
            .into());
        }

        let mut claims = Map::new();
        claims.insert(
            "user_cart_confirmation_required".to_string(),
            json!(mandate.user_cart_confirmation_required),
        );
        claims.insert("artifact_type".to_string(), json!(artifact.artifact_type));
        claims.insert("content_type".to_string(), json!(artifact.content_type));
        claims.insert(CRYPTOGRAPHICALLY_VERIFIED_CLAIM.to_string(), json!(false));
        Ok(VerifiedAuthorization::new("intent_authorization", claims))
    }

    async fn verify_payment_authorization(
        &self,
        mandate: &PaymentMandate,
        artifact: &AuthorizationArtifact,
    ) -> Result<VerifiedAuthorization> {
        if artifact.value.trim().is_empty() {
            return Err(Ap2Error::MissingUserAuthorization {
                payment_mandate_id: mandate.payment_mandate_contents.payment_mandate_id.clone(),
            }
            .into());
        }

        let mut claims = Map::new();
        claims.insert(
            "payment_mandate_id".to_string(),
            json!(mandate.payment_mandate_contents.payment_mandate_id),
        );
        claims.insert("artifact_type".to_string(), json!(artifact.artifact_type));
        claims.insert("content_type".to_string(), json!(artifact.content_type));
        claims.insert(CRYPTOGRAPHICALLY_VERIFIED_CLAIM.to_string(), json!(false));
        Ok(VerifiedAuthorization::new("user_authorization", claims))
    }
}
