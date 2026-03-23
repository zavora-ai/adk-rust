use std::fmt;

use adk_core::AdkIdentity;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use crate::domain::{
    Cart, CommerceActor, EvidenceReference, FulfillmentSelection, InterventionState, MerchantRef,
    OrderSnapshot, OrderState, PaymentProcessorRef, ProtocolDescriptor, ProtocolExtensionEnvelope,
    ProtocolExtensions, ReceiptState,
};
use crate::kernel::PaymentsKernelError;

/// Canonical transaction identifier shared across all adapters.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TransactionId(pub String);

impl TransactionId {
    /// Returns the transaction identifier as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for TransactionId {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl From<String> for TransactionId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl fmt::Display for TransactionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Distinguishes user-present and pre-authorized deferred commerce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommerceMode {
    HumanPresent,
    HumanNotPresent,
}

/// Canonical transaction state machine shared by ACP and AP2 adapters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionState {
    Draft,
    Negotiating,
    AwaitingUserAuthorization,
    AwaitingPaymentMethod,
    InterventionRequired(Box<InterventionState>),
    Authorized,
    Completed,
    Canceled,
    Failed,
}

/// Lightweight summary tag for canonical transaction state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionStateTag {
    Draft,
    Negotiating,
    AwaitingUserAuthorization,
    AwaitingPaymentMethod,
    InterventionRequired,
    Authorized,
    Completed,
    Canceled,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransactionPhase {
    Draft,
    Negotiating,
    AwaitingUserAuthorization,
    AwaitingPaymentMethod,
    InterventionRequired,
    Authorized,
    Completed,
    Canceled,
    Failed,
}

impl TransactionPhase {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Negotiating => "negotiating",
            Self::AwaitingUserAuthorization => "awaiting_user_authorization",
            Self::AwaitingPaymentMethod => "awaiting_payment_method",
            Self::InterventionRequired => "intervention_required",
            Self::Authorized => "authorized",
            Self::Completed => "completed",
            Self::Canceled => "canceled",
            Self::Failed => "failed",
        }
    }
}

impl TransactionState {
    fn phase(&self) -> TransactionPhase {
        match self {
            Self::Draft => TransactionPhase::Draft,
            Self::Negotiating => TransactionPhase::Negotiating,
            Self::AwaitingUserAuthorization => TransactionPhase::AwaitingUserAuthorization,
            Self::AwaitingPaymentMethod => TransactionPhase::AwaitingPaymentMethod,
            Self::InterventionRequired(_) => TransactionPhase::InterventionRequired,
            Self::Authorized => TransactionPhase::Authorized,
            Self::Completed => TransactionPhase::Completed,
            Self::Canceled => TransactionPhase::Canceled,
            Self::Failed => TransactionPhase::Failed,
        }
    }

    /// Returns `true` when the transition is allowed by the canonical
    /// transaction state machine.
    #[must_use]
    pub fn can_transition_to(&self, next: &Self) -> bool {
        use TransactionPhase::{
            Authorized, AwaitingPaymentMethod, AwaitingUserAuthorization, Canceled, Completed,
            Draft, Failed, InterventionRequired, Negotiating,
        };

        match (self.phase(), next.phase()) {
            (from, to) if from == to => true,
            (Draft, Negotiating | Canceled | Failed) => true,
            (
                Negotiating,
                AwaitingUserAuthorization
                | AwaitingPaymentMethod
                | InterventionRequired
                | Canceled
                | Failed,
            ) => true,
            (
                AwaitingUserAuthorization,
                AwaitingPaymentMethod | InterventionRequired | Canceled | Failed,
            ) => true,
            (AwaitingPaymentMethod, Authorized | InterventionRequired | Canceled | Failed) => true,
            (
                InterventionRequired,
                AwaitingUserAuthorization | AwaitingPaymentMethod | Authorized | Canceled | Failed,
            ) => true,
            (Authorized, Completed | Canceled | Failed) => true,
            _ => false,
        }
    }

    /// Returns `true` when no further canonical payment progress is expected.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Canceled | Self::Failed)
    }

    /// Returns the state tag without carrying the full transition payload.
    #[must_use]
    pub fn tag(&self) -> TransactionStateTag {
        match self {
            Self::Draft => TransactionStateTag::Draft,
            Self::Negotiating => TransactionStateTag::Negotiating,
            Self::AwaitingUserAuthorization => TransactionStateTag::AwaitingUserAuthorization,
            Self::AwaitingPaymentMethod => TransactionStateTag::AwaitingPaymentMethod,
            Self::InterventionRequired(_) => TransactionStateTag::InterventionRequired,
            Self::Authorized => TransactionStateTag::Authorized,
            Self::Completed => TransactionStateTag::Completed,
            Self::Canceled => TransactionStateTag::Canceled,
            Self::Failed => TransactionStateTag::Failed,
        }
    }
}

/// Canonical reference to a payment-method selection or delegated credential.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentMethodSelection {
    pub selection_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_hint: Option<String>,
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
}

/// Additional protocol reference that does not fit a well-known canonical slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolReference {
    pub protocol: ProtocolDescriptor,
    pub reference_kind: String,
    pub reference_value: String,
}

/// Correlated protocol identifiers for one canonical transaction.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolRefs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acp_checkout_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acp_order_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acp_delegate_payment_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ap2_intent_mandate_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ap2_cart_mandate_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ap2_payment_mandate_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ap2_payment_receipt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional: Vec<ProtocolReference>,
}

/// Safe digest metadata for one raw protocol artifact stored outside transcript
/// and memory surfaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolEnvelopeDigest {
    pub evidence_ref: EvidenceReference,
    pub created_at: DateTime<Utc>,
}

impl ProtocolEnvelopeDigest {
    /// Creates a digest wrapper for one stored evidence reference.
    #[must_use]
    pub fn new(evidence_ref: EvidenceReference, created_at: DateTime<Utc>) -> Self {
        Self { evidence_ref, created_at }
    }
}

/// Masked transaction summary safe for transcript and memory surfaces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SafeTransactionSummary {
    pub transaction_id: TransactionId,
    pub merchant_name: String,
    pub item_titles: Vec<String>,
    pub total: crate::domain::Money,
    pub mode: CommerceMode,
    pub state: TransactionStateTag,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_state: Option<OrderState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_state: Option<ReceiptState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_required_action: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protocol_tags: Vec<String>,
    pub updated_at: DateTime<Utc>,
}

impl SafeTransactionSummary {
    /// Derives a safe summary from canonical transaction state.
    #[must_use]
    pub fn from_record(record: &TransactionRecord) -> Self {
        let mut protocol_tags = BTreeSet::new();

        for digest in &record.evidence_digests {
            let descriptor = &digest.evidence_ref.protocol;
            let tag = descriptor.version.as_ref().map_or_else(
                || descriptor.name.clone(),
                |version| format!("{}@{version}", descriptor.name),
            );
            protocol_tags.insert(tag);
        }

        for envelope in record.extensions.as_slice() {
            let descriptor = &envelope.protocol;
            let tag = descriptor.version.as_ref().map_or_else(
                || descriptor.name.clone(),
                |version| format!("{}@{version}", descriptor.name),
            );
            protocol_tags.insert(tag);
        }

        let next_required_action = match &record.state {
            TransactionState::Draft | TransactionState::Negotiating => {
                Some("continue checkout negotiation".to_string())
            }
            TransactionState::AwaitingUserAuthorization => {
                Some("obtain explicit user authorization".to_string())
            }
            TransactionState::AwaitingPaymentMethod => {
                Some("collect or delegate a payment method".to_string())
            }
            TransactionState::InterventionRequired(intervention) => intervention
                .instructions
                .clone()
                .or_else(|| Some("complete the required payment intervention".to_string())),
            TransactionState::Authorized => {
                Some("await order completion or settlement".to_string())
            }
            TransactionState::Completed | TransactionState::Canceled | TransactionState::Failed => {
                None
            }
        };

        Self {
            transaction_id: record.transaction_id.clone(),
            merchant_name: record
                .merchant_of_record
                .display_name
                .clone()
                .unwrap_or_else(|| record.merchant_of_record.legal_name.clone()),
            item_titles: record.cart.lines.iter().map(|line| line.title.clone()).collect(),
            total: record.cart.total.clone(),
            mode: record.mode,
            state: record.state.tag(),
            order_state: record.order.as_ref().map(|order| order.state),
            receipt_state: record.order.as_ref().map(|order| order.receipt_state),
            next_required_action,
            protocol_tags: protocol_tags.into_iter().collect(),
            updated_at: record.last_updated_at,
        }
    }

    /// Returns a safe one-line summary suitable for transcript surfaces.
    #[must_use]
    pub fn transcript_text(&self) -> String {
        let items = if self.item_titles.is_empty() {
            "items unavailable".to_string()
        } else {
            self.item_titles.join(", ")
        };

        let next = self
            .next_required_action
            .as_ref()
            .map_or_else(String::new, |action| format!(" Next action: {action}."));

        format!(
            "Transaction {} with {} is {:?} for {} {}. Items: {}.{}",
            self.transaction_id,
            self.merchant_name,
            self.state,
            self.total.amount_minor,
            self.total.currency,
            items,
            next
        )
    }
}

/// Durable protocol-neutral transaction record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransactionRecord {
    pub transaction_id: TransactionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_identity: Option<AdkIdentity>,
    pub initiated_by: CommerceActor,
    pub merchant_of_record: MerchantRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payment_processor: Option<PaymentProcessorRef>,
    pub mode: CommerceMode,
    pub state: TransactionState,
    pub cart: Cart,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fulfillment: Option<FulfillmentSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<OrderSnapshot>,
    #[serde(default)]
    pub protocol_refs: ProtocolRefs,
    #[serde(default, skip_serializing_if = "ProtocolExtensions::is_empty")]
    pub extensions: ProtocolExtensions,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<EvidenceReference>,
    pub safe_summary: SafeTransactionSummary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_digests: Vec<ProtocolEnvelopeDigest>,
    pub created_at: DateTime<Utc>,
    pub last_updated_at: DateTime<Utc>,
}

impl TransactionRecord {
    /// Creates a new canonical transaction record in the `draft` state.
    #[must_use]
    pub fn new(
        transaction_id: TransactionId,
        initiated_by: CommerceActor,
        merchant_of_record: MerchantRef,
        mode: CommerceMode,
        cart: Cart,
        created_at: DateTime<Utc>,
    ) -> Self {
        let total = cart.total.clone();
        let mut record = Self {
            transaction_id,
            session_identity: None,
            initiated_by,
            merchant_of_record,
            payment_processor: None,
            mode,
            state: TransactionState::Draft,
            cart,
            fulfillment: None,
            order: None,
            protocol_refs: ProtocolRefs::default(),
            extensions: ProtocolExtensions::default(),
            evidence_refs: Vec::new(),
            safe_summary: SafeTransactionSummary {
                transaction_id: TransactionId::from("pending"),
                merchant_name: String::new(),
                item_titles: Vec::new(),
                total,
                mode,
                state: TransactionStateTag::Draft,
                order_state: None,
                receipt_state: None,
                next_required_action: None,
                protocol_tags: Vec::new(),
                updated_at: created_at,
            },
            evidence_digests: Vec::new(),
            created_at,
            last_updated_at: created_at,
        };
        record.recompute_safe_summary();
        record
    }

    /// Applies one canonical transaction-state transition.
    ///
    /// # Errors
    ///
    /// Returns an error when the transition skips or rewinds the canonical
    /// payment lifecycle.
    pub fn transition_to(
        &mut self,
        next: TransactionState,
        updated_at: DateTime<Utc>,
    ) -> std::result::Result<(), PaymentsKernelError> {
        if !self.state.can_transition_to(&next) {
            return Err(PaymentsKernelError::InvalidTransactionTransition {
                from: self.state.phase().as_str(),
                to: next.phase().as_str(),
            });
        }

        self.state = next;
        self.last_updated_at = updated_at;
        self.recompute_safe_summary();
        Ok(())
    }

    /// Attaches one protocol extension envelope without discarding its original
    /// fields.
    pub fn attach_extension(&mut self, envelope: ProtocolExtensionEnvelope) {
        self.extensions.push(envelope);
        self.recompute_safe_summary();
    }

    /// Attaches one evidence reference to the transaction record.
    pub fn attach_evidence_ref(&mut self, evidence_ref: EvidenceReference) {
        self.evidence_refs.push(evidence_ref);
    }

    /// Attaches a safe digest for a stored protocol artifact.
    pub fn attach_evidence_digest(&mut self, digest: ProtocolEnvelopeDigest) {
        self.evidence_digests.push(digest);
        self.recompute_safe_summary();
    }

    /// Recomputes the safe summary after canonical state changes.
    pub fn recompute_safe_summary(&mut self) {
        self.safe_summary = SafeTransactionSummary::from_record(self);
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use serde_json::{Map, json};

    use super::*;
    use crate::domain::{
        CartLine, FulfillmentKind, InterventionKind, InterventionStatus, Money, OrderState,
        ReceiptState,
    };

    fn sample_actor() -> CommerceActor {
        CommerceActor {
            actor_id: "shopper-agent".to_string(),
            role: crate::domain::CommerceActorRole::AgentSurface,
            display_name: Some("shopper".to_string()),
            tenant_id: Some("tenant-1".to_string()),
            extensions: ProtocolExtensions::default(),
        }
    }

    fn sample_merchant() -> MerchantRef {
        MerchantRef {
            merchant_id: "merchant-123".to_string(),
            legal_name: "Merchant Example LLC".to_string(),
            display_name: Some("Merchant Example".to_string()),
            statement_descriptor: Some("MERCHANT*EXAMPLE".to_string()),
            country_code: Some("US".to_string()),
            website: Some("https://merchant.example".to_string()),
            extensions: ProtocolExtensions::default(),
        }
    }

    fn sample_cart() -> Cart {
        Cart {
            cart_id: Some("cart-1".to_string()),
            lines: vec![CartLine {
                line_id: "line-1".to_string(),
                merchant_sku: Some("sku-123".to_string()),
                title: "Widget".to_string(),
                quantity: 1,
                unit_price: Money::new("USD", 1_500, 2),
                total_price: Money::new("USD", 1_500, 2),
                product_class: Some("widgets".to_string()),
                extensions: ProtocolExtensions::default(),
            }],
            subtotal: Some(Money::new("USD", 1_500, 2)),
            adjustments: Vec::new(),
            total: Money::new("USD", 1_500, 2),
            affiliate_attribution: None,
            extensions: ProtocolExtensions::default(),
        }
    }

    fn sample_record() -> TransactionRecord {
        TransactionRecord::new(
            TransactionId::from("tx-123"),
            sample_actor(),
            sample_merchant(),
            CommerceMode::HumanPresent,
            sample_cart(),
            Utc.with_ymd_and_hms(2026, 3, 22, 10, 0, 0).unwrap(),
        )
    }

    #[test]
    fn transaction_state_machine_allows_happy_path() {
        let mut record = sample_record();

        record
            .transition_to(
                TransactionState::Negotiating,
                Utc.with_ymd_and_hms(2026, 3, 22, 10, 5, 0).unwrap(),
            )
            .unwrap();
        record
            .transition_to(
                TransactionState::AwaitingPaymentMethod,
                Utc.with_ymd_and_hms(2026, 3, 22, 10, 6, 0).unwrap(),
            )
            .unwrap();
        record
            .transition_to(
                TransactionState::Authorized,
                Utc.with_ymd_and_hms(2026, 3, 22, 10, 7, 0).unwrap(),
            )
            .unwrap();
        record
            .transition_to(
                TransactionState::Completed,
                Utc.with_ymd_and_hms(2026, 3, 22, 10, 8, 0).unwrap(),
            )
            .unwrap();

        assert_eq!(record.state, TransactionState::Completed);
        assert!(record.state.is_terminal());
    }

    #[test]
    fn transaction_state_machine_rejects_skipped_transition() {
        let mut record = sample_record();
        let err = record
            .transition_to(
                TransactionState::Completed,
                Utc.with_ymd_and_hms(2026, 3, 22, 10, 5, 0).unwrap(),
            )
            .unwrap_err();

        assert_eq!(
            err,
            PaymentsKernelError::InvalidTransactionTransition { from: "draft", to: "completed" }
        );
        assert_eq!(record.state, TransactionState::Draft);
    }

    #[test]
    fn order_and_receipt_state_machines_enforce_progression() {
        let mut order = OrderSnapshot {
            order_id: Some("order-1".to_string()),
            receipt_id: None,
            state: OrderState::Draft,
            receipt_state: ReceiptState::NotRequested,
            extensions: ProtocolExtensions::default(),
        };

        order.transition_order_state(OrderState::PendingPayment).unwrap();
        order.transition_order_state(OrderState::Authorized).unwrap();
        order.transition_receipt_state(ReceiptState::Pending).unwrap();
        order.transition_receipt_state(ReceiptState::Authorized).unwrap();

        let err = order.transition_order_state(OrderState::Refunded).unwrap_err();
        assert_eq!(
            err,
            PaymentsKernelError::InvalidOrderTransition {
                from: OrderState::Authorized,
                to: OrderState::Refunded,
            }
        );
    }

    #[test]
    fn protocol_extensions_round_trip_without_loss() {
        let mut record = sample_record();
        record.fulfillment = Some(FulfillmentSelection {
            fulfillment_id: "ship-1".to_string(),
            kind: FulfillmentKind::Shipping,
            label: "Standard".to_string(),
            amount: Some(Money::new("USD", 300, 2)),
            destination: None,
            requires_user_selection: false,
            extensions: ProtocolExtensions::default(),
        });
        record.order = Some(OrderSnapshot {
            order_id: Some("order-2".to_string()),
            receipt_id: Some("receipt-2".to_string()),
            state: OrderState::PendingPayment,
            receipt_state: ReceiptState::Pending,
            extensions: ProtocolExtensions::default(),
        });
        record.state = TransactionState::InterventionRequired(Box::new(InterventionState {
            intervention_id: "int-1".to_string(),
            kind: InterventionKind::ThreeDsChallenge,
            status: InterventionStatus::Pending,
            instructions: Some("Complete 3DS".to_string()),
            continuation_token: Some("continue-1".to_string()),
            requested_by: None,
            expires_at: None,
            extensions: ProtocolExtensions::default(),
        }));

        let evidence_ref = EvidenceReference {
            evidence_id: "ev-1".to_string(),
            protocol: ProtocolDescriptor::acp("2026-01-30"),
            artifact_kind: "checkout_session".to_string(),
            digest: Some("sha256:abc".to_string()),
        };

        let mut acp_fields = Map::new();
        acp_fields.insert("paymentHandler".to_string(), json!({"type": "card"}));
        acp_fields.insert("affiliateAttribution".to_string(), json!({"partnerId": "aff-1"}));

        let mut ap2_fields = Map::new();
        ap2_fields.insert("cartMandate".to_string(), json!({"id": "cm-1"}));
        ap2_fields.insert("riskData".to_string(), json!({"score": 42}));

        record.attach_extension(ProtocolExtensionEnvelope {
            protocol: ProtocolDescriptor::acp("2026-01-30"),
            fields: acp_fields,
            evidence_refs: vec![evidence_ref.clone()],
        });
        record.attach_extension(ProtocolExtensionEnvelope {
            protocol: ProtocolDescriptor::ap2("v0.1-alpha"),
            fields: ap2_fields,
            evidence_refs: vec![EvidenceReference {
                evidence_id: "ev-2".to_string(),
                protocol: ProtocolDescriptor::ap2("v0.1-alpha"),
                artifact_kind: "payment_mandate".to_string(),
                digest: Some("sha256:def".to_string()),
            }],
        });
        record.attach_evidence_ref(evidence_ref);

        let encoded = serde_json::to_value(&record).unwrap();
        let decoded: TransactionRecord = serde_json::from_value(encoded).unwrap();

        assert_eq!(decoded.extensions.as_slice().len(), 2);
        assert_eq!(
            decoded.extensions.as_slice()[0].fields["paymentHandler"],
            json!({"type": "card"})
        );
        assert_eq!(decoded.extensions.as_slice()[1].fields["cartMandate"], json!({"id": "cm-1"}));
        assert_eq!(decoded.evidence_refs.len(), 1);
    }
}
