use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::domain::TransactionRecord;

use super::{PaymentOperation, PaymentsAuthError, check_payment_operation_scopes};

/// Authenticated request identity carried into payment tools or endpoints.
///
/// This type intentionally keeps request identity separate from the durable
/// session identity stored on a transaction and from the protocol actor roles
/// recorded in commerce payloads.
///
/// # Example
///
/// ```
/// use adk_payments::auth::{AuthenticatedPaymentRequest, PaymentOperation};
///
/// let request = AuthenticatedPaymentRequest::new("alice")
///     .with_tenant_id("tenant-1")
///     .with_scopes(["payments:checkout:update"]);
///
/// request
///     .check_operation_scopes(PaymentOperation::UpdateCheckout)
///     .unwrap();
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthenticatedPaymentRequest {
    pub user_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub metadata: Map<String, Value>,
}

impl AuthenticatedPaymentRequest {
    /// Creates a new authenticated payment request capsule.
    #[must_use]
    pub fn new(user_id: impl Into<String>) -> Self {
        Self {
            user_id: user_id.into(),
            session_id: None,
            tenant_id: None,
            scopes: Vec::new(),
            metadata: Map::new(),
        }
    }

    /// Attaches the caller's session identifier.
    #[must_use]
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Attaches the caller's tenant identifier.
    #[must_use]
    pub fn with_tenant_id(mut self, tenant_id: impl Into<String>) -> Self {
        self.tenant_id = Some(tenant_id.into());
        self
    }

    /// Replaces the granted scope list.
    #[must_use]
    pub fn with_scopes<I, S>(mut self, scopes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.scopes = scopes.into_iter().map(Into::into).collect();
        self
    }

    /// Adds one metadata field preserved alongside the authenticated request.
    #[must_use]
    pub fn with_metadata(mut self, key: impl Into<String>, value: Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    /// Checks that the request scopes authorize one payment operation.
    ///
    /// # Errors
    ///
    /// Returns [`PaymentsAuthError::MissingScopes`] when the request is missing
    /// one or more required scopes.
    pub fn check_operation_scopes(
        &self,
        operation: PaymentOperation,
    ) -> Result<(), PaymentsAuthError> {
        check_payment_operation_scopes(operation, &self.scopes)
    }

    /// Rejects attempts to access a transaction with conflicting identity or
    /// tenant bindings.
    ///
    /// # Errors
    ///
    /// Returns [`PaymentsAuthError::IdentityConflict`] when the authenticated
    /// request tries to rebind the durable session or tenant association of an
    /// existing transaction.
    pub fn assert_transaction_binding(
        &self,
        record: &TransactionRecord,
    ) -> Result<(), PaymentsAuthError> {
        if let Some(session_identity) = &record.session_identity {
            let expected_user = session_identity.user_id.as_ref();
            if self.user_id != expected_user {
                return Err(PaymentsAuthError::IdentityConflict {
                    transaction_id: record.transaction_id.to_string(),
                    binding: "session_user_id",
                    expected: expected_user.to_string(),
                    actual: self.user_id.clone(),
                });
            }

            if let Some(session_id) = &self.session_id
                && session_id != session_identity.session_id.as_ref()
            {
                return Err(PaymentsAuthError::IdentityConflict {
                    transaction_id: record.transaction_id.to_string(),
                    binding: "session_id",
                    expected: session_identity.session_id.to_string(),
                    actual: session_id.clone(),
                });
            }
        }

        if let Some(request_tenant_id) = &self.tenant_id
            && let Some(expected_tenant_id) = &record.initiated_by.tenant_id
            && request_tenant_id != expected_tenant_id
        {
            return Err(PaymentsAuthError::IdentityConflict {
                transaction_id: record.transaction_id.to_string(),
                binding: "tenant_id",
                expected: expected_tenant_id.clone(),
                actual: request_tenant_id.clone(),
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    use super::*;
    use crate::domain::{
        Cart, CartLine, CommerceActor, CommerceActorRole, CommerceMode, MerchantRef, Money,
        ProtocolExtensions, TransactionId,
    };

    fn sample_transaction() -> TransactionRecord {
        let created_at = Utc.with_ymd_and_hms(2026, 3, 22, 12, 0, 0).unwrap();
        let mut record = TransactionRecord::new(
            TransactionId::from("tx-tenant"),
            CommerceActor {
                actor_id: "shopper-agent".to_string(),
                role: CommerceActorRole::AgentSurface,
                display_name: Some("shopper".to_string()),
                tenant_id: Some("tenant-1".to_string()),
                extensions: ProtocolExtensions::default(),
            },
            MerchantRef {
                merchant_id: "merchant-123".to_string(),
                legal_name: "Merchant Example LLC".to_string(),
                display_name: Some("Merchant Example".to_string()),
                statement_descriptor: None,
                country_code: Some("US".to_string()),
                website: Some("https://merchant.example".to_string()),
                extensions: ProtocolExtensions::default(),
            },
            CommerceMode::HumanPresent,
            Cart {
                cart_id: Some("cart-1".to_string()),
                lines: vec![CartLine {
                    line_id: "line-1".to_string(),
                    merchant_sku: Some("sku-1".to_string()),
                    title: "Widget".to_string(),
                    quantity: 1,
                    unit_price: Money::new("USD", 2_500, 2),
                    total_price: Money::new("USD", 2_500, 2),
                    product_class: Some("widgets".to_string()),
                    extensions: ProtocolExtensions::default(),
                }],
                subtotal: Some(Money::new("USD", 2_500, 2)),
                adjustments: Vec::new(),
                total: Money::new("USD", 2_500, 2),
                affiliate_attribution: None,
                extensions: ProtocolExtensions::default(),
            },
            created_at,
        );
        record.session_identity = Some(adk_core::AdkIdentity::new(
            adk_core::AppName::try_from("commerce-app").unwrap(),
            adk_core::UserId::try_from("alice").unwrap(),
            adk_core::SessionId::try_from("session-123").unwrap(),
        ));
        record
    }

    #[test]
    fn request_builder_replaces_scopes_and_metadata() {
        let request = AuthenticatedPaymentRequest::new("alice")
            .with_session_id("session-123")
            .with_tenant_id("tenant-1")
            .with_scopes(["payments:checkout:update"])
            .with_metadata("channel", json!("agent"));

        assert_eq!(request.user_id, "alice");
        assert_eq!(request.session_id.as_deref(), Some("session-123"));
        assert_eq!(request.tenant_id.as_deref(), Some("tenant-1"));
        assert_eq!(request.scopes, vec!["payments:checkout:update".to_string()]);
        assert_eq!(request.metadata.get("channel"), Some(&json!("agent")));
    }

    #[test]
    fn binding_check_rejects_tenant_rebinding() {
        let record = sample_transaction();
        let err = AuthenticatedPaymentRequest::new("alice")
            .with_session_id("session-123")
            .with_tenant_id("tenant-2")
            .assert_transaction_binding(&record)
            .unwrap_err();

        match err {
            PaymentsAuthError::IdentityConflict { transaction_id, binding, expected, actual } => {
                assert_eq!(transaction_id, "tx-tenant");
                assert_eq!(binding, "tenant_id");
                assert_eq!(expected, "tenant-1");
                assert_eq!(actual, "tenant-2");
            }
            other => panic!("unexpected auth error: {other}"),
        }
    }
}
