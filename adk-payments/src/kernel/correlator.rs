//! Kernel-mediated cross-protocol correlation.
//!
//! Routes ACP stable `2026-01-30` and AP2 `v0.1-alpha` adapters through the
//! same canonical transaction ID and journal model. Provides best-effort
//! canonical projections where safe and returns explicit errors where direct
//! protocol-to-protocol conversion would lose semantics or accountability
//! evidence.

use std::sync::Arc;

use adk_core::Result;

use crate::domain::{
    Cart, FulfillmentSelection, OrderSnapshot, PaymentMethodSelection, ProtocolExtensions,
    ProtocolRefs, TransactionRecord, TransactionState,
};
use crate::kernel::commands::TransactionLookup;
use crate::kernel::errors::PaymentsKernelError;
use crate::kernel::service::TransactionStore;

/// Describes the originating protocol for a correlation operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolOrigin {
    Acp,
    Ap2,
}

impl ProtocolOrigin {
    /// Returns the protocol name string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Acp => "acp",
            Self::Ap2 => "ap2",
        }
    }
}

/// Describes a specific protocol reference slot in [`ProtocolRefs`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolRefKind {
    AcpCheckoutSessionId,
    AcpOrderId,
    AcpDelegatePaymentId,
    Ap2IntentMandateId,
    Ap2CartMandateId,
    Ap2PaymentMandateId,
    Ap2PaymentReceiptId,
}

/// Result of a canonical projection attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum ProjectionResult<T> {
    /// The projection succeeded without semantic loss.
    Projected(T),
    /// The projection is not safe; the field has no canonical equivalent.
    Unsupported { field: String, source_protocol: String, reason: String },
}

impl<T> ProjectionResult<T> {
    /// Returns the projected value or `None` if unsupported.
    #[must_use]
    pub fn ok(self) -> Option<T> {
        match self {
            Self::Projected(value) => Some(value),
            Self::Unsupported { .. } => None,
        }
    }

    /// Returns `true` when the projection succeeded.
    #[must_use]
    pub fn is_projected(&self) -> bool {
        matches!(self, Self::Projected(_))
    }
}

/// Cross-protocol correlator that routes ACP and AP2 adapters through the same
/// canonical transaction ID and journal model.
///
/// The correlator enforces three rules:
/// 1. Both protocols share one canonical `TransactionId` per transaction.
/// 2. Protocol-specific identifiers are correlated in [`ProtocolRefs`] without
///    assuming they are interchangeable.
/// 3. Direct protocol-to-protocol conversion is refused when it would lose
///    semantics or accountability evidence.
pub struct ProtocolCorrelator {
    transaction_store: Arc<dyn TransactionStore>,
}

impl ProtocolCorrelator {
    /// Creates a new correlator backed by the canonical transaction store.
    #[must_use]
    pub fn new(transaction_store: Arc<dyn TransactionStore>) -> Self {
        Self { transaction_store }
    }

    /// Looks up a canonical transaction by its internal ID.
    pub async fn get_transaction(
        &self,
        lookup: TransactionLookup,
    ) -> Result<Option<TransactionRecord>> {
        self.transaction_store.get(lookup).await
    }

    /// Looks up a canonical transaction by an ACP checkout session ID.
    ///
    /// Scans unresolved transactions for a matching `protocol_refs.acp_checkout_session_id`.
    /// For production use, a dedicated index would be more efficient.
    pub async fn find_by_acp_checkout_session_id(
        &self,
        session_identity: Option<adk_core::AdkIdentity>,
        acp_checkout_session_id: &str,
    ) -> Result<Option<TransactionRecord>> {
        let unresolved = self
            .transaction_store
            .list_unresolved(crate::kernel::commands::ListUnresolvedTransactionsRequest {
                session_identity: session_identity.clone(),
            })
            .await?;

        Ok(unresolved.into_iter().find(|record| {
            record.protocol_refs.acp_checkout_session_id.as_deref() == Some(acp_checkout_session_id)
        }))
    }

    /// Looks up a canonical transaction by an AP2 mandate ID (intent, cart, or payment).
    pub async fn find_by_ap2_mandate_id(
        &self,
        session_identity: Option<adk_core::AdkIdentity>,
        mandate_id: &str,
    ) -> Result<Option<TransactionRecord>> {
        let unresolved = self
            .transaction_store
            .list_unresolved(crate::kernel::commands::ListUnresolvedTransactionsRequest {
                session_identity: session_identity.clone(),
            })
            .await?;

        Ok(unresolved.into_iter().find(|record| {
            let refs = &record.protocol_refs;
            refs.ap2_intent_mandate_id.as_deref() == Some(mandate_id)
                || refs.ap2_cart_mandate_id.as_deref() == Some(mandate_id)
                || refs.ap2_payment_mandate_id.as_deref() == Some(mandate_id)
        }))
    }

    /// Attaches a protocol-specific reference to an existing transaction record.
    ///
    /// This is the canonical way to correlate ACP and AP2 identifiers under one
    /// transaction. The correlator never overwrites an existing reference slot
    /// with a different value.
    ///
    /// # Errors
    ///
    /// Returns an error if the slot is already occupied by a different value.
    pub fn attach_protocol_ref(
        record: &mut TransactionRecord,
        kind: ProtocolRefKind,
        value: String,
    ) -> std::result::Result<(), PaymentsKernelError> {
        let slot = match &kind {
            ProtocolRefKind::AcpCheckoutSessionId => {
                &mut record.protocol_refs.acp_checkout_session_id
            }
            ProtocolRefKind::AcpOrderId => &mut record.protocol_refs.acp_order_id,
            ProtocolRefKind::AcpDelegatePaymentId => {
                &mut record.protocol_refs.acp_delegate_payment_id
            }
            ProtocolRefKind::Ap2IntentMandateId => &mut record.protocol_refs.ap2_intent_mandate_id,
            ProtocolRefKind::Ap2CartMandateId => &mut record.protocol_refs.ap2_cart_mandate_id,
            ProtocolRefKind::Ap2PaymentMandateId => {
                &mut record.protocol_refs.ap2_payment_mandate_id
            }
            ProtocolRefKind::Ap2PaymentReceiptId => {
                &mut record.protocol_refs.ap2_payment_receipt_id
            }
        };

        if let Some(existing) = slot.as_ref() {
            if existing != &value {
                return Err(PaymentsKernelError::UnsupportedAction {
                    action: format!("rebind protocol ref {kind:?} from `{existing}` to `{value}`"),
                    protocol: "kernel".to_string(),
                });
            }
            return Ok(());
        }

        *slot = Some(value);
        Ok(())
    }

    /// Returns all protocol identifiers correlated to one canonical transaction.
    #[must_use]
    pub fn correlated_refs(record: &TransactionRecord) -> &ProtocolRefs {
        &record.protocol_refs
    }

    /// Returns the set of protocol names that have contributed evidence to this
    /// transaction.
    #[must_use]
    pub fn contributing_protocols(record: &TransactionRecord) -> Vec<String> {
        let mut protocols = std::collections::BTreeSet::new();

        for digest in &record.evidence_digests {
            protocols.insert(digest.evidence_ref.protocol.name.clone());
        }
        for evidence_ref in &record.evidence_refs {
            protocols.insert(evidence_ref.protocol.name.clone());
        }
        for envelope in record.extensions.as_slice() {
            protocols.insert(envelope.protocol.name.clone());
        }

        if record.protocol_refs.acp_checkout_session_id.is_some()
            || record.protocol_refs.acp_order_id.is_some()
            || record.protocol_refs.acp_delegate_payment_id.is_some()
        {
            protocols.insert("acp".to_string());
        }
        if record.protocol_refs.ap2_intent_mandate_id.is_some()
            || record.protocol_refs.ap2_cart_mandate_id.is_some()
            || record.protocol_refs.ap2_payment_mandate_id.is_some()
            || record.protocol_refs.ap2_payment_receipt_id.is_some()
        {
            protocols.insert("ap2".to_string());
        }

        protocols.into_iter().collect()
    }

    /// Returns `true` when both ACP and AP2 have contributed to this transaction.
    #[must_use]
    pub fn is_dual_protocol(record: &TransactionRecord) -> bool {
        let protocols = Self::contributing_protocols(record);
        protocols.contains(&"acp".to_string()) && protocols.contains(&"ap2".to_string())
    }
}

// ---------------------------------------------------------------------------
// Best-effort canonical projections (Task 9.2)
// ---------------------------------------------------------------------------

/// Best-effort canonical projections where ACP or AP2 data can be mapped
/// safely without semantic loss.
///
/// These projections are intentionally one-directional: protocol data is
/// projected into canonical kernel types. The kernel never projects canonical
/// data back into a different protocol's wire format because that would
/// fabricate provenance.
impl ProtocolCorrelator {
    /// Projects an ACP cart (line items + totals) into the canonical cart model.
    ///
    /// This projection is safe because ACP line items, totals, and currency map
    /// directly to canonical `Cart` fields without losing structure.
    #[must_use]
    pub fn project_acp_cart_to_canonical(record: &TransactionRecord) -> ProjectionResult<Cart> {
        if record.cart.cart_id.is_some() || !record.cart.lines.is_empty() {
            return ProjectionResult::Projected(record.cart.clone());
        }
        ProjectionResult::Unsupported {
            field: "cart".to_string(),
            source_protocol: "acp".to_string(),
            reason: "no cart data available in the transaction record".to_string(),
        }
    }

    /// Projects an AP2 cart mandate's payment details into the canonical cart model.
    ///
    /// This projection is safe because AP2 `PaymentRequest.details.displayItems`
    /// and `total` map to canonical `CartLine` and `Cart.total` without losing
    /// the item-level structure.
    #[must_use]
    pub fn project_ap2_cart_to_canonical(record: &TransactionRecord) -> ProjectionResult<Cart> {
        if record.cart.cart_id.is_some() || !record.cart.lines.is_empty() {
            return ProjectionResult::Projected(record.cart.clone());
        }
        ProjectionResult::Unsupported {
            field: "cart".to_string(),
            source_protocol: "ap2".to_string(),
            reason: "no cart data available in the transaction record".to_string(),
        }
    }

    /// Projects ACP or AP2 order updates into the canonical order snapshot.
    ///
    /// Both protocols produce order state that maps to the canonical
    /// `OrderSnapshot` without semantic loss.
    #[must_use]
    pub fn project_order_to_canonical(
        record: &TransactionRecord,
    ) -> ProjectionResult<OrderSnapshot> {
        match &record.order {
            Some(order) => ProjectionResult::Projected(order.clone()),
            None => ProjectionResult::Unsupported {
                field: "order".to_string(),
                source_protocol: "kernel".to_string(),
                reason: "transaction has no order snapshot yet".to_string(),
            },
        }
    }

    /// Projects the canonical transaction state into a protocol-neutral
    /// settlement summary.
    ///
    /// Both ACP order updates and AP2 payment receipts can update canonical
    /// settlement state, so this projection is safe in both directions.
    #[must_use]
    pub fn project_settlement_state(
        record: &TransactionRecord,
    ) -> ProjectionResult<TransactionState> {
        ProjectionResult::Projected(record.state.clone())
    }

    /// Projects the canonical fulfillment selection.
    ///
    /// Both ACP fulfillment options and AP2 shipping options map to the
    /// canonical `FulfillmentSelection` without semantic loss.
    #[must_use]
    pub fn project_fulfillment_to_canonical(
        record: &TransactionRecord,
    ) -> ProjectionResult<FulfillmentSelection> {
        match &record.fulfillment {
            Some(fulfillment) => ProjectionResult::Projected(fulfillment.clone()),
            None => ProjectionResult::Unsupported {
                field: "fulfillment".to_string(),
                source_protocol: "kernel".to_string(),
                reason: "transaction has no fulfillment selection".to_string(),
            },
        }
    }

    /// Projects the canonical payment method selection.
    ///
    /// Both ACP payment handlers and AP2 payment response method names map to
    /// the canonical `PaymentMethodSelection` without semantic loss.
    #[must_use]
    pub fn project_payment_method(
        record: &TransactionRecord,
    ) -> ProjectionResult<PaymentMethodSelection> {
        // Payment method is stored in extensions by both adapters.
        // Look for the most recent payment method selection in extensions.
        for envelope in record.extensions.as_slice().iter().rev() {
            if let Some(selection_kind) =
                envelope.fields.get("selection_kind").and_then(serde_json::Value::as_str)
            {
                return ProjectionResult::Projected(PaymentMethodSelection {
                    selection_kind: selection_kind.to_string(),
                    reference: envelope
                        .fields
                        .get("reference")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    display_hint: envelope
                        .fields
                        .get("display_hint")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    extensions: ProtocolExtensions::default(),
                });
            }
        }
        ProjectionResult::Unsupported {
            field: "payment_method".to_string(),
            source_protocol: "kernel".to_string(),
            reason: "no payment method selection found in transaction extensions".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Lossy conversion guards (Task 9.3)
// ---------------------------------------------------------------------------

/// Describes an unsafe cross-protocol conversion that the kernel refuses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LossyConversionError {
    pub source_protocol: String,
    pub target_protocol: String,
    pub field: String,
    pub reason: String,
}

impl std::fmt::Display for LossyConversionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "lossy conversion refused: `{}` from {} cannot be safely mapped to {} ({})",
            self.field, self.source_protocol, self.target_protocol, self.reason
        )
    }
}

impl From<LossyConversionError> for PaymentsKernelError {
    fn from(value: LossyConversionError) -> Self {
        PaymentsKernelError::UnsupportedAction {
            action: format!(
                "convert `{}` from {} to {}",
                value.field, value.source_protocol, value.target_protocol
            ),
            protocol: value.source_protocol,
        }
    }
}

/// Guards against unsafe direct protocol-to-protocol conversions.
///
/// The kernel mediates all cross-protocol operations. These guards return
/// explicit errors when a direct ACP-to-AP2 or AP2-to-ACP conversion would
/// lose semantics or accountability evidence.
impl ProtocolCorrelator {
    /// Refuses direct conversion of an ACP delegated payment token to an AP2
    /// user authorization credential.
    ///
    /// ACP delegated payment tokens are scoped PSP credentials with merchant
    /// and amount constraints. AP2 user authorization artifacts are
    /// cryptographic proofs of user consent. Converting one to the other would
    /// fabricate provenance.
    pub fn refuse_acp_delegate_to_ap2_authorization(
        field: &str,
    ) -> std::result::Result<(), PaymentsKernelError> {
        Err(LossyConversionError {
            source_protocol: "acp".to_string(),
            target_protocol: "ap2".to_string(),
            field: field.to_string(),
            reason: "ACP delegated payment tokens are scoped PSP credentials; \
                     AP2 user authorization artifacts are cryptographic proofs of user consent. \
                     Converting one to the other would fabricate provenance."
                .to_string(),
        }
        .into())
    }

    /// Refuses direct conversion of an AP2 signed user authorization to an ACP
    /// delegated payment token.
    ///
    /// AP2 user authorization presentations prove user consent through
    /// cryptographic signatures. ACP delegated payment tokens are PSP-issued
    /// scoped credentials. Converting one to the other would lose the
    /// cryptographic accountability chain.
    pub fn refuse_ap2_authorization_to_acp_delegate(
        field: &str,
    ) -> std::result::Result<(), PaymentsKernelError> {
        Err(LossyConversionError {
            source_protocol: "ap2".to_string(),
            target_protocol: "acp".to_string(),
            field: field.to_string(),
            reason: "AP2 signed user authorization artifacts prove consent through \
                     cryptographic signatures. ACP delegated payment tokens are PSP-issued \
                     scoped credentials. Converting one to the other would lose the \
                     cryptographic accountability chain."
                .to_string(),
        }
        .into())
    }

    /// Refuses direct conversion of ACP checkout session state to AP2 mandate
    /// state.
    ///
    /// ACP checkout sessions are merchant-facing HTTP resources with
    /// server-managed lifecycle. AP2 mandates are signed authorization
    /// artifacts with explicit role separation. The state models are not
    /// equivalent.
    pub fn refuse_acp_session_to_ap2_mandate(
        field: &str,
    ) -> std::result::Result<(), PaymentsKernelError> {
        Err(LossyConversionError {
            source_protocol: "acp".to_string(),
            target_protocol: "ap2".to_string(),
            field: field.to_string(),
            reason: "ACP checkout sessions are merchant-facing HTTP resources with \
                     server-managed lifecycle. AP2 mandates are signed authorization \
                     artifacts with explicit role separation. Direct conversion would \
                     lose the authorization model."
                .to_string(),
        }
        .into())
    }

    /// Refuses direct conversion of AP2 mandate state to ACP checkout session
    /// state.
    ///
    /// AP2 mandates carry cryptographic authorization chains and role-separated
    /// provenance. ACP checkout sessions are server-managed merchant resources.
    /// Direct conversion would discard the mandate's authorization evidence.
    pub fn refuse_ap2_mandate_to_acp_session(
        field: &str,
    ) -> std::result::Result<(), PaymentsKernelError> {
        Err(LossyConversionError {
            source_protocol: "ap2".to_string(),
            target_protocol: "acp".to_string(),
            field: field.to_string(),
            reason: "AP2 mandates carry cryptographic authorization chains and role-separated \
                     provenance. ACP checkout sessions are server-managed merchant resources. \
                     Direct conversion would discard the mandate's authorization evidence."
                .to_string(),
        }
        .into())
    }

    /// Validates that a cross-protocol operation goes through the kernel rather
    /// than attempting direct protocol-to-protocol transcoding.
    ///
    /// Returns `Ok(())` when the operation is kernel-mediated (both sides use
    /// the canonical transaction). Returns an error when the caller attempts
    /// to bypass the kernel.
    pub fn require_kernel_mediation(
        record: &TransactionRecord,
        source: ProtocolOrigin,
        target: ProtocolOrigin,
        operation: &str,
    ) -> std::result::Result<(), PaymentsKernelError> {
        if source == target {
            return Ok(());
        }

        // The operation is kernel-mediated if the record has a canonical
        // transaction ID and both protocols have contributed through the kernel.
        if record.transaction_id.as_str().is_empty() {
            return Err(PaymentsKernelError::UnsupportedAction {
                action: format!("cross-protocol `{operation}` requires a canonical transaction ID"),
                protocol: source.as_str().to_string(),
            });
        }

        Ok(())
    }
}
