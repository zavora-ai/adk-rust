use std::sync::Arc;

use adk_auth::{AuditEvent, AuditEventType, AuditOutcome, AuditSink};
use chrono::Utc;
use serde_json::json;

use crate::domain::{ProtocolDescriptor, TransactionRecord};

use super::{AuthenticatedPaymentRequest, PaymentOperation, PaymentsAuthError};

/// Emits structured payment audit events through an `adk-auth` audit sink.
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::Arc;
///
/// use adk_auth::{AuditOutcome, FileAuditSink};
/// use adk_payments::auth::{AuthenticatedPaymentRequest, PaymentAuditor, PaymentOperation};
/// use adk_payments::domain::ProtocolDescriptor;
///
/// # let sink = Arc::new(FileAuditSink::new("/tmp/payments-audit.jsonl").unwrap());
/// let auditor = PaymentAuditor::new(sink);
/// let request = AuthenticatedPaymentRequest::new("alice");
///
/// # let transaction = todo!("load transaction");
/// auditor
///     .record_operation(
///         &request,
///         &transaction,
///         &ProtocolDescriptor::acp("2026-01-30"),
///         PaymentOperation::CompleteCheckout,
///         AuditOutcome::Allowed,
///         false,
///     )
///     .await
///     .unwrap();
/// ```
#[derive(Clone)]
pub struct PaymentAuditor {
    sink: Arc<dyn AuditSink>,
}

impl PaymentAuditor {
    /// Creates a payment auditor backed by the provided sink.
    #[must_use]
    pub fn new(sink: Arc<dyn AuditSink>) -> Self {
        Self { sink }
    }

    /// Records one sensitive payment operation with structured metadata.
    ///
    /// # Errors
    ///
    /// Returns [`PaymentsAuthError::AuditSink`] when the underlying sink fails.
    pub async fn record_operation(
        &self,
        request: &AuthenticatedPaymentRequest,
        record: &TransactionRecord,
        protocol: &ProtocolDescriptor,
        operation: PaymentOperation,
        outcome: AuditOutcome,
        intervention_occurred: bool,
    ) -> Result<(), PaymentsAuthError> {
        let event = payment_audit_event(
            request,
            record,
            protocol,
            operation,
            &outcome,
            intervention_occurred,
        );
        self.sink.log(event).await.map_err(PaymentsAuthError::from)
    }
}

fn payment_audit_event(
    request: &AuthenticatedPaymentRequest,
    record: &TransactionRecord,
    protocol: &ProtocolDescriptor,
    operation: PaymentOperation,
    outcome: &AuditOutcome,
    intervention_occurred: bool,
) -> AuditEvent {
    let metadata = json!({
        "operation": operation.as_str(),
        "transactionId": record.transaction_id.as_str(),
        "protocol": protocol.name,
        "protocolVersion": protocol.version,
        "merchantOfRecord": {
            "merchantId": record.merchant_of_record.merchant_id,
            "legalName": record.merchant_of_record.legal_name,
            "displayName": record.merchant_of_record.display_name,
        },
        "outcome": audit_outcome_name(outcome),
        "interventionOccurred": intervention_occurred,
        "authenticatedRequest": {
            "userId": request.user_id,
            "sessionId": request.session_id,
            "tenantId": request.tenant_id,
            "scopes": request.scopes,
            "metadata": request.metadata,
        },
        "sessionIdentity": record.session_identity.as_ref().map(|_identity| json!({
            "present": true,
        })),
        "protocolActor": {
            "actorId": record.initiated_by.actor_id,
            "role": record.initiated_by.role,
            "displayName": record.initiated_by.display_name,
            "tenantId": record.initiated_by.tenant_id,
        },
    });

    AuditEvent {
        timestamp: Utc::now(),
        user: request.user_id.clone(),
        session_id: None, // redacted — sensitive identifier
        event_type: AuditEventType::PermissionCheck,
        resource: operation.audit_resource().to_string(),
        outcome: outcome.clone(),
        metadata: Some(metadata),
    }
}

fn audit_outcome_name(outcome: &AuditOutcome) -> &'static str {
    match outcome {
        AuditOutcome::Allowed => "allowed",
        AuditOutcome::Denied => "denied",
        AuditOutcome::Error => "error",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use adk_auth::AuthError;
    use async_trait::async_trait;
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    use super::*;
    use crate::domain::{
        Cart, CartLine, CommerceActor, CommerceActorRole, CommerceMode, MerchantRef, Money,
        ProtocolExtensions, TransactionId,
    };

    struct RecordingAuditSink {
        events: Mutex<Vec<AuditEvent>>,
    }

    impl RecordingAuditSink {
        fn new() -> Self {
            Self { events: Mutex::new(Vec::new()) }
        }

        fn recorded_events(&self) -> Vec<AuditEvent> {
            self.events.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
        }
    }

    #[async_trait]
    impl AuditSink for RecordingAuditSink {
        async fn log(&self, event: AuditEvent) -> Result<(), AuthError> {
            self.events.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(event);
            Ok(())
        }
    }

    fn sample_transaction() -> TransactionRecord {
        let created_at = Utc.with_ymd_and_hms(2026, 3, 22, 14, 0, 0).unwrap();
        let mut record = TransactionRecord::new(
            TransactionId::from("tx-audit"),
            CommerceActor {
                actor_id: "merchant-agent".to_string(),
                role: CommerceActorRole::Merchant,
                display_name: Some("merchant".to_string()),
                tenant_id: Some("tenant-1".to_string()),
                extensions: ProtocolExtensions::default(),
            },
            MerchantRef {
                merchant_id: "merchant-123".to_string(),
                legal_name: "Merchant Example LLC".to_string(),
                display_name: Some("Merchant Example".to_string()),
                statement_descriptor: Some("MERCHANT*EXAMPLE".to_string()),
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
                    unit_price: Money::new("USD", 3_500, 2),
                    total_price: Money::new("USD", 3_500, 2),
                    product_class: Some("widgets".to_string()),
                    extensions: ProtocolExtensions::default(),
                }],
                subtotal: Some(Money::new("USD", 3_500, 2)),
                adjustments: Vec::new(),
                total: Money::new("USD", 3_500, 2),
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

    #[tokio::test]
    async fn auditor_emits_structured_payment_metadata() {
        let sink = Arc::new(RecordingAuditSink::new());
        let auditor = PaymentAuditor::new(sink.clone());
        let request = AuthenticatedPaymentRequest::new("alice")
            .with_session_id("session-123")
            .with_tenant_id("tenant-1")
            .with_scopes(["payments:checkout:complete"])
            .with_metadata("channel", json!("agent"));

        auditor
            .record_operation(
                &request,
                &sample_transaction(),
                &ProtocolDescriptor::acp("2026-01-30"),
                PaymentOperation::CompleteCheckout,
                AuditOutcome::Allowed,
                true,
            )
            .await
            .unwrap();

        let events = sink.recorded_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].resource, "payments.checkout.complete");
        assert_eq!(events[0].user, "alice");
        assert_eq!(events[0].session_id, None);

        let metadata = events[0].metadata.as_ref().unwrap();
        assert_eq!(metadata["transactionId"], "tx-audit");
        assert_eq!(metadata["protocol"], "acp");
        assert_eq!(metadata["protocolVersion"], "2026-01-30");
        assert_eq!(metadata["operation"], "checkout_complete");
        assert_eq!(metadata["interventionOccurred"], true);
        assert_eq!(metadata["authenticatedRequest"]["tenantId"], "tenant-1");
        assert_eq!(metadata["protocolActor"]["actorId"], "merchant-agent");
        assert_eq!(metadata["sessionIdentity"]["present"], true);
    }
}
