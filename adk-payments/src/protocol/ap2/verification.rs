use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use adk_core::Result;
use async_trait::async_trait;

use crate::protocol::ap2::error::Ap2Error;
use crate::protocol::ap2::types::{
    AuthorizationArtifact, CartMandate, IntentMandate, PaymentMandate,
};

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
    fn new(artifact_kind: impl Into<String>, claims: Map<String, Value>) -> Self {
        Self { artifact_kind: artifact_kind.into(), verified_at: Utc::now(), claims }
    }
}

/// Verifies merchant authorization artifacts bound to `CartMandate`.
#[async_trait]
pub trait MerchantAuthorizationVerifier: Send + Sync {
    async fn verify_cart_authorization(
        &self,
        mandate: &CartMandate,
        artifact: &AuthorizationArtifact,
    ) -> Result<VerifiedAuthorization>;
}

/// Verifies user authorization artifacts bound to intent or payment mandates.
#[async_trait]
pub trait UserAuthorizationVerifier: Send + Sync {
    async fn verify_intent_authorization(
        &self,
        mandate: &IntentMandate,
        artifact: &AuthorizationArtifact,
    ) -> Result<VerifiedAuthorization>;

    async fn verify_payment_authorization(
        &self,
        mandate: &PaymentMandate,
        artifact: &AuthorizationArtifact,
    ) -> Result<VerifiedAuthorization>;
}

/// Minimal verifier that requires a non-empty merchant authorization artifact.
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
        Ok(VerifiedAuthorization::new("merchant_authorization", claims))
    }
}

/// Minimal verifier that requires non-empty detached or embedded user authorization.
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
        Ok(VerifiedAuthorization::new("user_authorization", claims))
    }
}
