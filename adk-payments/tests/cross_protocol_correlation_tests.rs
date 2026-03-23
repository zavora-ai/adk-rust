//! Regression tests for a merchant backend serving both ACP and AP2 adapters
//! in one deployment through the kernel-mediated cross-protocol correlator.
//!
//! Validates Requirements: 7.1, 7.2, 7.3, 7.4, 13.1, 14.6, 15.5

use std::collections::HashMap;
use std::sync::Arc;

use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use adk_payments::domain::{
    Cart, CartLine, CommerceActor, CommerceActorRole, CommerceMode, MerchantRef, Money,
    OrderSnapshot, OrderState, ProtocolDescriptor, ProtocolExtensionEnvelope, ProtocolExtensions,
    ReceiptState, TransactionId, TransactionRecord, TransactionState,
};
use adk_payments::journal::SessionBackedTransactionStore;
use adk_payments::kernel::{
    PaymentsKernelError, ProtocolCorrelator, ProtocolOrigin, ProtocolRefKind, TransactionLookup,
    TransactionStore,
};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use chrono::{TimeZone, Utc};

async fn create_identity(session_service: &InMemorySessionService) -> AdkIdentity {
    let identity = AdkIdentity::new(
        AppName::try_from("payments-app").unwrap(),
        UserId::try_from("user-1").unwrap(),
        SessionId::try_from("session-1").unwrap(),
    );

    session_service
        .create(CreateRequest {
            app_name: identity.app_name.as_ref().to_string(),
            user_id: identity.user_id.as_ref().to_string(),
            session_id: Some(identity.session_id.as_ref().to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();

    identity
}

fn sample_actor() -> CommerceActor {
    CommerceActor {
        actor_id: "shopper-agent".to_string(),
        role: CommerceActorRole::AgentSurface,
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

fn sample_record(transaction_id: &str, identity: &AdkIdentity) -> TransactionRecord {
    let mut record = TransactionRecord::new(
        TransactionId::from(transaction_id),
        sample_actor(),
        sample_merchant(),
        CommerceMode::HumanPresent,
        sample_cart(),
        Utc.with_ymd_and_hms(2026, 3, 22, 10, 0, 0).unwrap(),
    );
    record.session_identity = Some(identity.clone());
    record
}

// ---------------------------------------------------------------------------
// Requirement 7.1: One internal transaction identifier per canonical transaction
// ---------------------------------------------------------------------------

#[tokio::test]
async fn assigns_one_canonical_transaction_id_shared_across_protocols() {
    let session_service = Arc::new(InMemorySessionService::new());
    let identity = create_identity(session_service.as_ref()).await;
    let store: Arc<dyn TransactionStore> =
        Arc::new(SessionBackedTransactionStore::new(session_service));
    let correlator = ProtocolCorrelator::new(store.clone());

    let mut record = sample_record("tx-dual-1", &identity);

    // ACP adapter attaches its checkout session ID
    ProtocolCorrelator::attach_protocol_ref(
        &mut record,
        ProtocolRefKind::AcpCheckoutSessionId,
        "cs_acp_123".to_string(),
    )
    .unwrap();

    // AP2 adapter attaches its cart mandate ID to the same transaction
    ProtocolCorrelator::attach_protocol_ref(
        &mut record,
        ProtocolRefKind::Ap2CartMandateId,
        "cm_ap2_456".to_string(),
    )
    .unwrap();

    store.upsert(record.clone()).await.unwrap();

    // Both protocol refs are correlated under one canonical transaction ID
    let refs = ProtocolCorrelator::correlated_refs(&record);
    assert_eq!(refs.acp_checkout_session_id.as_deref(), Some("cs_acp_123"));
    assert_eq!(refs.ap2_cart_mandate_id.as_deref(), Some("cm_ap2_456"));
    assert_eq!(record.transaction_id.as_str(), "tx-dual-1");

    // Lookup by canonical ID returns the same record
    let found = correlator
        .get_transaction(TransactionLookup {
            transaction_id: TransactionId::from("tx-dual-1"),
            session_identity: Some(identity.clone()),
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.protocol_refs.acp_checkout_session_id.as_deref(), Some("cs_acp_123"));
    assert_eq!(found.protocol_refs.ap2_cart_mandate_id.as_deref(), Some("cm_ap2_456"));
}

// ---------------------------------------------------------------------------
// Requirement 7.2: Correlate ACP and AP2 identifiers under one transaction
// ---------------------------------------------------------------------------

#[tokio::test]
async fn correlates_all_protocol_identifiers_under_one_transaction() {
    let session_service = Arc::new(InMemorySessionService::new());
    let identity = create_identity(session_service.as_ref()).await;
    let store: Arc<dyn TransactionStore> =
        Arc::new(SessionBackedTransactionStore::new(session_service));

    let mut record = sample_record("tx-full-refs", &identity);

    // Attach all ACP refs
    ProtocolCorrelator::attach_protocol_ref(
        &mut record,
        ProtocolRefKind::AcpCheckoutSessionId,
        "cs_100".to_string(),
    )
    .unwrap();
    ProtocolCorrelator::attach_protocol_ref(
        &mut record,
        ProtocolRefKind::AcpOrderId,
        "ord_200".to_string(),
    )
    .unwrap();
    ProtocolCorrelator::attach_protocol_ref(
        &mut record,
        ProtocolRefKind::AcpDelegatePaymentId,
        "dp_300".to_string(),
    )
    .unwrap();

    // Attach all AP2 refs
    ProtocolCorrelator::attach_protocol_ref(
        &mut record,
        ProtocolRefKind::Ap2IntentMandateId,
        "im_400".to_string(),
    )
    .unwrap();
    ProtocolCorrelator::attach_protocol_ref(
        &mut record,
        ProtocolRefKind::Ap2CartMandateId,
        "cm_500".to_string(),
    )
    .unwrap();
    ProtocolCorrelator::attach_protocol_ref(
        &mut record,
        ProtocolRefKind::Ap2PaymentMandateId,
        "pm_600".to_string(),
    )
    .unwrap();
    ProtocolCorrelator::attach_protocol_ref(
        &mut record,
        ProtocolRefKind::Ap2PaymentReceiptId,
        "pr_700".to_string(),
    )
    .unwrap();

    store.upsert(record.clone()).await.unwrap();

    let refs = ProtocolCorrelator::correlated_refs(&record);
    assert_eq!(refs.acp_checkout_session_id.as_deref(), Some("cs_100"));
    assert_eq!(refs.acp_order_id.as_deref(), Some("ord_200"));
    assert_eq!(refs.acp_delegate_payment_id.as_deref(), Some("dp_300"));
    assert_eq!(refs.ap2_intent_mandate_id.as_deref(), Some("im_400"));
    assert_eq!(refs.ap2_cart_mandate_id.as_deref(), Some("cm_500"));
    assert_eq!(refs.ap2_payment_mandate_id.as_deref(), Some("pm_600"));
    assert_eq!(refs.ap2_payment_receipt_id.as_deref(), Some("pr_700"));
}

// ---------------------------------------------------------------------------
// Requirement 7.2: Protocol identifier lookup
// ---------------------------------------------------------------------------

#[tokio::test]
async fn finds_transaction_by_acp_checkout_session_id() {
    let session_service = Arc::new(InMemorySessionService::new());
    let identity = create_identity(session_service.as_ref()).await;
    let store: Arc<dyn TransactionStore> =
        Arc::new(SessionBackedTransactionStore::new(session_service));
    let correlator = ProtocolCorrelator::new(store.clone());

    let mut record = sample_record("tx-acp-lookup", &identity);
    ProtocolCorrelator::attach_protocol_ref(
        &mut record,
        ProtocolRefKind::AcpCheckoutSessionId,
        "cs_lookup_test".to_string(),
    )
    .unwrap();
    store.upsert(record).await.unwrap();

    let found = correlator
        .find_by_acp_checkout_session_id(Some(identity.clone()), "cs_lookup_test")
        .await
        .unwrap();
    assert!(found.is_some());
    assert_eq!(found.unwrap().transaction_id.as_str(), "tx-acp-lookup");

    let not_found =
        correlator.find_by_acp_checkout_session_id(Some(identity), "cs_nonexistent").await.unwrap();
    assert!(not_found.is_none());
}

#[tokio::test]
async fn finds_transaction_by_ap2_mandate_id() {
    let session_service = Arc::new(InMemorySessionService::new());
    let identity = create_identity(session_service.as_ref()).await;
    let store: Arc<dyn TransactionStore> =
        Arc::new(SessionBackedTransactionStore::new(session_service));
    let correlator = ProtocolCorrelator::new(store.clone());

    let mut record = sample_record("tx-ap2-lookup", &identity);
    ProtocolCorrelator::attach_protocol_ref(
        &mut record,
        ProtocolRefKind::Ap2CartMandateId,
        "cm_lookup_test".to_string(),
    )
    .unwrap();
    store.upsert(record).await.unwrap();

    let found =
        correlator.find_by_ap2_mandate_id(Some(identity.clone()), "cm_lookup_test").await.unwrap();
    assert!(found.is_some());
    assert_eq!(found.unwrap().transaction_id.as_str(), "tx-ap2-lookup");
}

// ---------------------------------------------------------------------------
// Requirement 7.3: Both adapters operate against the same canonical state
// ---------------------------------------------------------------------------

#[tokio::test]
async fn both_adapters_operate_against_same_canonical_cart_and_order_state() {
    let session_service = Arc::new(InMemorySessionService::new());
    let identity = create_identity(session_service.as_ref()).await;
    let store: Arc<dyn TransactionStore> =
        Arc::new(SessionBackedTransactionStore::new(session_service));

    let mut record = sample_record("tx-shared-state", &identity);

    // ACP adapter creates the checkout and attaches its session ID
    ProtocolCorrelator::attach_protocol_ref(
        &mut record,
        ProtocolRefKind::AcpCheckoutSessionId,
        "cs_shared".to_string(),
    )
    .unwrap();
    record.attach_extension(
        ProtocolExtensionEnvelope::new(ProtocolDescriptor::acp("2026-01-30"))
            .with_field("operation", serde_json::json!("create_checkout_session")),
    );

    // AP2 adapter attaches its cart mandate to the same transaction
    ProtocolCorrelator::attach_protocol_ref(
        &mut record,
        ProtocolRefKind::Ap2CartMandateId,
        "cm_shared".to_string(),
    )
    .unwrap();
    record.attach_extension(
        ProtocolExtensionEnvelope::new(ProtocolDescriptor::ap2("v0.1-alpha"))
            .with_field("artifact_kind", serde_json::json!("cart_mandate")),
    );

    // Both protocols share the same canonical cart
    record.cart.lines[0].title = "Shared Widget".to_string();
    record.cart.total = Money::new("USD", 2_000, 2);

    // Transition through the canonical state machine
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

    // Add order from ACP completion
    record.order = Some(OrderSnapshot {
        order_id: Some("ord_shared".to_string()),
        receipt_id: None,
        state: OrderState::Authorized,
        receipt_state: ReceiptState::Authorized,
        extensions: ProtocolExtensions::default(),
    });

    record
        .transition_to(
            TransactionState::Completed,
            Utc.with_ymd_and_hms(2026, 3, 22, 10, 8, 0).unwrap(),
        )
        .unwrap();

    store.upsert(record.clone()).await.unwrap();

    // Verify both protocols contributed
    assert!(ProtocolCorrelator::is_dual_protocol(&record));
    let protocols = ProtocolCorrelator::contributing_protocols(&record);
    assert!(protocols.contains(&"acp".to_string()));
    assert!(protocols.contains(&"ap2".to_string()));

    // Verify canonical projections work for the shared state
    let cart = ProtocolCorrelator::project_acp_cart_to_canonical(&record);
    assert!(cart.is_projected());
    assert_eq!(cart.ok().unwrap().lines[0].title, "Shared Widget");

    let order = ProtocolCorrelator::project_order_to_canonical(&record);
    assert!(order.is_projected());
    assert_eq!(order.ok().unwrap().order_id.as_deref(), Some("ord_shared"));

    let state = ProtocolCorrelator::project_settlement_state(&record);
    assert!(state.is_projected());
    assert_eq!(state.ok().unwrap(), TransactionState::Completed);
}

// ---------------------------------------------------------------------------
// Requirement 7.4: Refuse lossy direct protocol-to-protocol conversion
// ---------------------------------------------------------------------------

#[test]
fn refuses_acp_delegate_token_to_ap2_authorization() {
    let result =
        ProtocolCorrelator::refuse_acp_delegate_to_ap2_authorization("delegated_payment_token");
    assert!(result.is_err());
    let err = result.unwrap_err();
    match err {
        PaymentsKernelError::UnsupportedAction { action, protocol } => {
            assert!(action.contains("delegated_payment_token"));
            assert!(action.contains("acp"));
            assert!(action.contains("ap2"));
            assert_eq!(protocol, "acp");
        }
        other => panic!("expected UnsupportedAction, got {other:?}"),
    }
}

#[test]
fn refuses_ap2_authorization_to_acp_delegate_token() {
    let result =
        ProtocolCorrelator::refuse_ap2_authorization_to_acp_delegate("user_authorization_artifact");
    assert!(result.is_err());
    let err = result.unwrap_err();
    match err {
        PaymentsKernelError::UnsupportedAction { action, protocol } => {
            assert!(action.contains("user_authorization_artifact"));
            assert!(action.contains("ap2"));
            assert!(action.contains("acp"));
            assert_eq!(protocol, "ap2");
        }
        other => panic!("expected UnsupportedAction, got {other:?}"),
    }
}

#[test]
fn refuses_acp_session_to_ap2_mandate_conversion() {
    let result = ProtocolCorrelator::refuse_acp_session_to_ap2_mandate("checkout_session_state");
    assert!(result.is_err());
    let err = result.unwrap_err();
    match err {
        PaymentsKernelError::UnsupportedAction { action, protocol } => {
            assert!(action.contains("checkout_session_state"));
            assert_eq!(protocol, "acp");
        }
        other => panic!("expected UnsupportedAction, got {other:?}"),
    }
}

#[test]
fn refuses_ap2_mandate_to_acp_session_conversion() {
    let result = ProtocolCorrelator::refuse_ap2_mandate_to_acp_session("cart_mandate_state");
    assert!(result.is_err());
    let err = result.unwrap_err();
    match err {
        PaymentsKernelError::UnsupportedAction { action, protocol } => {
            assert!(action.contains("cart_mandate_state"));
            assert_eq!(protocol, "ap2");
        }
        other => panic!("expected UnsupportedAction, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Requirement 7.4: Kernel mediation enforcement
// ---------------------------------------------------------------------------

#[test]
fn requires_kernel_mediation_for_cross_protocol_operations() {
    let record = TransactionRecord::new(
        TransactionId::from("tx-mediation"),
        sample_actor(),
        sample_merchant(),
        CommerceMode::HumanPresent,
        sample_cart(),
        Utc.with_ymd_and_hms(2026, 3, 22, 10, 0, 0).unwrap(),
    );

    // Same-protocol operations are always allowed
    let result = ProtocolCorrelator::require_kernel_mediation(
        &record,
        ProtocolOrigin::Acp,
        ProtocolOrigin::Acp,
        "update_checkout",
    );
    assert!(result.is_ok());

    // Cross-protocol operations are allowed when the record has a canonical ID
    let result = ProtocolCorrelator::require_kernel_mediation(
        &record,
        ProtocolOrigin::Acp,
        ProtocolOrigin::Ap2,
        "correlate_order",
    );
    assert!(result.is_ok());

    // Cross-protocol operations fail when the record has no canonical ID
    let empty_record = TransactionRecord::new(
        TransactionId::from(""),
        sample_actor(),
        sample_merchant(),
        CommerceMode::HumanPresent,
        sample_cart(),
        Utc.with_ymd_and_hms(2026, 3, 22, 10, 0, 0).unwrap(),
    );
    let result = ProtocolCorrelator::require_kernel_mediation(
        &empty_record,
        ProtocolOrigin::Acp,
        ProtocolOrigin::Ap2,
        "correlate_order",
    );
    assert!(result.is_err());
}
