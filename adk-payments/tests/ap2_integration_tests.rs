#![cfg(feature = "ap2")]

mod support;

use adk_payments::AP2_ALPHA_BASELINE;
use adk_payments::domain::{
    CommerceMode, InterventionKind, InterventionStatus, OrderState, ProtocolDescriptor,
    ReceiptState, TransactionState, TransactionStateTag,
};
use adk_payments::protocol::ap2::{
    Ap2Adapter, AuthorizationArtifact, CartMandate, IntentMandate, PaymentMandate, PaymentReceipt,
};
use chrono::{Duration, Utc};
use serde_json::json;
use support::commerce_harness::{
    HarnessActorKind, MultiActorHarness, MultiActorHarnessActors, MultiActorHarnessConfig,
};

fn future_expiry() -> String {
    (Utc::now() + Duration::hours(1)).to_rfc3339()
}

fn make_cart_mandate(details_id: &str) -> CartMandate {
    serde_json::from_value(json!({
        "contents": {
            "id": details_id,
            "user_cart_confirmation_required": false,
            "payment_request": {
                "method_data": [{"supported_methods": "CARD"}],
                "details": {
                    "id": details_id,
                    "display_items": [{
                        "label": "Running Shoes",
                        "amount": {"currency": "USD", "value": 120.0}
                    }],
                    "total": {
                        "label": "Total",
                        "amount": {"currency": "USD", "value": 120.0}
                    }
                }
            },
            "cart_expiry": future_expiry(),
            "merchant_name": "AP2 Merchant"
        },
        "merchant_authorization": "signed-by-merchant",
        "timestamp": Utc::now().to_rfc3339()
    }))
    .expect("cart mandate JSON should be valid")
}

fn make_payment_mandate(details_id: &str, user_auth: Option<String>) -> PaymentMandate {
    serde_json::from_value(json!({
        "payment_mandate_contents": {
            "payment_mandate_id": format!("pm-{details_id}"),
            "payment_details_id": details_id,
            "payment_details_total": {
                "label": "Total",
                "amount": {"currency": "USD", "value": 120.0}
            },
            "payment_response": {
                "request_id": details_id,
                "method_name": "CARD"
            },
            "merchant_agent": "AP2 Merchant",
            "timestamp": Utc::now().to_rfc3339()
        },
        "user_authorization": user_auth
    }))
    .expect("payment mandate JSON should be valid")
}

fn make_success_receipt(payment_mandate_id: &str) -> PaymentReceipt {
    serde_json::from_value(json!({
        "payment_mandate_id": payment_mandate_id,
        "timestamp": Utc::now().to_rfc3339(),
        "payment_id": format!("pay-{payment_mandate_id}"),
        "amount": {"currency": "USD", "value": 120.0},
        "payment_status": {
            "merchant_confirmation_id": "conf-123",
            "psp_confirmation_id": "psp-456"
        }
    }))
    .expect("payment receipt JSON should be valid")
}

fn touch_acp_harness_api() {
    let _ = MultiActorHarnessConfig::acp_defaults;
    let _ = MultiActorHarness::webhook_context;
    let _ = MultiActorHarness::acp_context_template;
    let _ = MultiActorHarness::session_events_dump;
    let _ = |actors: &MultiActorHarnessActors| {
        let _ = &actors.webhook;
    };
}

#[tokio::test]
async fn test_ap2_human_present_shopper_merchant_payment_processor_journey() {
    touch_acp_harness_api();
    let harness = MultiActorHarness::new(MultiActorHarnessConfig::ap2_defaults()).await;
    let adapter = Ap2Adapter::new(
        harness.backend.clone(),
        harness.backend.clone(),
        harness.backend.clone(),
        harness.backend.clone(),
    );

    let tx_id = "hp-tx-001";
    let details_id = "hp-details-001";
    let merchant_context = harness.merchant_context(
        tx_id,
        CommerceMode::HumanPresent,
        ProtocolDescriptor::ap2(AP2_ALPHA_BASELINE),
    );
    let shopper_context = harness.shopper_context(
        tx_id,
        CommerceMode::HumanPresent,
        ProtocolDescriptor::ap2(AP2_ALPHA_BASELINE),
    );
    let processor_context = harness.payment_processor_context(
        tx_id,
        CommerceMode::HumanPresent,
        ProtocolDescriptor::ap2(AP2_ALPHA_BASELINE),
    );

    let cart_mandate = make_cart_mandate(details_id);
    let cart_record = adapter
        .submit_cart_mandate(merchant_context, cart_mandate)
        .await
        .expect("submit_cart_mandate should succeed");

    assert_eq!(cart_record.transaction_id.as_str(), tx_id);
    assert_eq!(cart_record.state.tag(), TransactionStateTag::AwaitingPaymentMethod);
    assert!(
        cart_record.protocol_refs.ap2_cart_mandate_id.is_some(),
        "cart mandate ref should be populated"
    );
    assert!(
        !cart_record.evidence_refs.is_empty(),
        "evidence refs should be stored after cart mandate"
    );

    let user_authorization = harness
        .issue_credentials_provider_artifact(
            tx_id,
            "user_authorization",
            "user-signed-authorization",
        )
        .await;
    let payment_mandate = make_payment_mandate(details_id, Some(user_authorization));
    let payment_result = adapter
        .submit_payment_mandate(shopper_context, payment_mandate)
        .await
        .expect("submit_payment_mandate should succeed");

    assert_eq!(payment_result.outcome, adk_payments::kernel::PaymentExecutionOutcome::Completed);
    assert!(
        payment_result.transaction.protocol_refs.ap2_payment_mandate_id.is_some(),
        "payment mandate ref should be populated"
    );
    assert!(payment_result.transaction.order.is_some(), "order should be created after payment");

    let receipt = make_success_receipt(&format!("pm-{details_id}"));
    let final_record = adapter
        .apply_payment_receipt(processor_context, receipt)
        .await
        .expect("apply_payment_receipt should succeed");

    assert_eq!(final_record.state, TransactionState::Completed);
    assert!(final_record.state.is_terminal());
    assert!(
        final_record.protocol_refs.ap2_payment_receipt_id.is_some(),
        "receipt ref should be populated"
    );
    assert_eq!(
        final_record.order.as_ref().unwrap().state,
        OrderState::Completed,
        "order should be completed after success receipt"
    );
    assert_eq!(
        final_record.order.as_ref().unwrap().receipt_state,
        ReceiptState::Settled,
        "receipt should be settled"
    );
    assert_eq!(final_record.payment_processor.as_ref().unwrap().processor_id, "ap2-processor");

    let evidence_kinds: Vec<&str> = final_record
        .evidence_refs
        .iter()
        .map(|reference| reference.artifact_kind.as_str())
        .collect();
    assert!(evidence_kinds.contains(&"cart_mandate"), "cart_mandate evidence should be stored");
    assert!(
        evidence_kinds.contains(&"merchant_authorization"),
        "merchant_authorization evidence should be stored"
    );
    assert!(
        evidence_kinds.contains(&"payment_mandate"),
        "payment_mandate evidence should be stored"
    );
    assert!(
        evidence_kinds.contains(&"user_authorization"),
        "user_authorization evidence should be stored"
    );
    assert!(
        evidence_kinds.contains(&"payment_receipt"),
        "payment_receipt evidence should be stored"
    );
    assert!(final_record.protocol_refs.ap2_cart_mandate_id.is_some());
    assert!(final_record.protocol_refs.ap2_payment_mandate_id.is_some());
    assert!(final_record.protocol_refs.ap2_payment_receipt_id.is_some());

    let state_dump = harness.session_state_dump().await;
    let memory_text = harness.memory_text(tx_id).await;
    assert!(!state_dump.contains("signed-by-merchant"));
    assert!(!state_dump.contains("user-signed-authorization"));
    assert!(!memory_text.contains("signed-by-merchant"));
    assert!(!memory_text.contains("user-signed-authorization"));
    assert!(memory_text.contains(tx_id));

    let raw_user_authorization = final_record
        .evidence_refs
        .iter()
        .find(|reference| reference.artifact_kind == "user_authorization")
        .cloned()
        .expect("user authorization evidence should exist");
    let stored_user_authorization = harness.load_evidence(&raw_user_authorization).await;
    assert_eq!(
        String::from_utf8(stored_user_authorization.body).unwrap(),
        "user-signed-authorization"
    );

    let actions = harness.recorded_actions().await;
    assert!(actions.iter().any(|action| {
        action.actor == HarnessActorKind::Merchant && action.action == "create_checkout"
    }));
    assert!(actions.iter().any(|action| {
        action.actor == HarnessActorKind::CredentialsProvider
            && action.action == "issue_user_authorization"
    }));
    assert!(actions.iter().any(|action| {
        action.actor == HarnessActorKind::Shopper && action.action == "execute_payment"
    }));
    assert!(actions.iter().any(|action| {
        action.actor == HarnessActorKind::PaymentProcessor
            && action.action == "sync_payment_outcome"
    }));
}

#[tokio::test]
async fn test_ap2_human_not_present_intent_autonomous_and_forced_return() {
    touch_acp_harness_api();
    let harness = MultiActorHarness::new(MultiActorHarnessConfig::ap2_defaults()).await;
    let adapter = Ap2Adapter::new(
        harness.backend.clone(),
        harness.backend.clone(),
        harness.backend.clone(),
        harness.backend.clone(),
    )
    .with_intervention_service(harness.backend.clone());

    let tx_id_auto = "hnp-auto-001";
    let details_id_auto = "hnp-details-auto-001";
    let shopper_context_auto = harness.shopper_context(
        tx_id_auto,
        CommerceMode::HumanNotPresent,
        ProtocolDescriptor::ap2(AP2_ALPHA_BASELINE),
    );
    let merchant_context_auto = harness.merchant_context(
        tx_id_auto,
        CommerceMode::HumanNotPresent,
        ProtocolDescriptor::ap2(AP2_ALPHA_BASELINE),
    );
    let processor_context_auto = harness.payment_processor_context(
        tx_id_auto,
        CommerceMode::HumanNotPresent,
        ProtocolDescriptor::ap2(AP2_ALPHA_BASELINE),
    );

    let intent_authorization_auto = harness
        .issue_credentials_provider_artifact(
            tx_id_auto,
            "intent_authorization",
            "signed-intent-token-abc",
        )
        .await;
    let intent_record = adapter
        .submit_intent_mandate(
            shopper_context_auto.clone(),
            IntentMandate {
                user_cart_confirmation_required: false,
                natural_language_description: "Buy running shoes under $200".to_string(),
                merchants: Some(vec!["AP2 Merchant".to_string()]),
                skus: None,
                requires_refundability: false,
                intent_expiry: future_expiry(),
            },
            Some(AuthorizationArtifact::new(
                "user_intent_authorization",
                intent_authorization_auto,
                "text/plain",
            )),
        )
        .await
        .expect("submit_intent_mandate should succeed");
    assert_eq!(intent_record.transaction_id.as_str(), tx_id_auto);
    assert!(
        intent_record.protocol_refs.ap2_intent_mandate_id.is_some(),
        "intent mandate ref should be populated"
    );

    let cart_record_auto = adapter
        .submit_cart_mandate(merchant_context_auto, make_cart_mandate(details_id_auto))
        .await
        .expect("submit_cart_mandate should succeed for autonomous flow");
    assert_eq!(cart_record_auto.state.tag(), TransactionStateTag::AwaitingPaymentMethod);

    let payment_result_auto = adapter
        .submit_payment_mandate(shopper_context_auto, make_payment_mandate(details_id_auto, None))
        .await
        .expect("autonomous payment should succeed without user auth");
    assert_eq!(
        payment_result_auto.outcome,
        adk_payments::kernel::PaymentExecutionOutcome::Completed,
        "autonomous payment should complete when intent allows it"
    );
    assert!(payment_result_auto.intervention.is_none());

    let final_auto = adapter
        .apply_payment_receipt(
            processor_context_auto,
            make_success_receipt(&format!("pm-{details_id_auto}")),
        )
        .await
        .expect("apply_payment_receipt should succeed");
    assert_eq!(final_auto.state, TransactionState::Completed);

    let tx_id_forced = "hnp-forced-001";
    let details_id_forced = "hnp-details-forced-001";
    let shopper_context_forced = harness.shopper_context(
        tx_id_forced,
        CommerceMode::HumanNotPresent,
        ProtocolDescriptor::ap2(AP2_ALPHA_BASELINE),
    );
    let merchant_context_forced = harness.merchant_context(
        tx_id_forced,
        CommerceMode::HumanNotPresent,
        ProtocolDescriptor::ap2(AP2_ALPHA_BASELINE),
    );

    let intent_authorization_forced = harness
        .issue_credentials_provider_artifact(
            tx_id_forced,
            "intent_authorization",
            "signed-intent-token-forced",
        )
        .await;
    let _intent_forced = adapter
        .submit_intent_mandate(
            shopper_context_forced.clone(),
            IntentMandate {
                user_cart_confirmation_required: true,
                natural_language_description: "Buy shoes but confirm with me first".to_string(),
                merchants: Some(vec!["AP2 Merchant".to_string()]),
                skus: None,
                requires_refundability: false,
                intent_expiry: future_expiry(),
            },
            Some(AuthorizationArtifact::new(
                "user_intent_authorization",
                intent_authorization_forced,
                "text/plain",
            )),
        )
        .await
        .expect("submit_intent_mandate should succeed for forced-return flow");

    let _cart_forced = adapter
        .submit_cart_mandate(merchant_context_forced, make_cart_mandate(details_id_forced))
        .await
        .expect("submit_cart_mandate should succeed for forced-return flow");

    let payment_result_forced = adapter
        .submit_payment_mandate(
            shopper_context_forced,
            make_payment_mandate(details_id_forced, None),
        )
        .await
        .expect("payment mandate should return intervention, not error");
    assert_eq!(
        payment_result_forced.outcome,
        adk_payments::kernel::PaymentExecutionOutcome::InterventionRequired,
        "should require intervention when user_cart_confirmation_required is true"
    );
    assert!(payment_result_forced.intervention.is_some(), "intervention state should be populated");

    let intervention = payment_result_forced.intervention.unwrap();
    assert_eq!(
        intervention.kind,
        InterventionKind::BuyerReconfirmation,
        "intervention kind should be BuyerReconfirmation"
    );
    assert_eq!(intervention.status, InterventionStatus::Pending);
    assert!(
        intervention.continuation_token.is_some(),
        "continuation token should be present for resumption"
    );

    let forced_record = harness.transaction(tx_id_forced).await;
    assert_eq!(
        forced_record.state.tag(),
        TransactionStateTag::InterventionRequired,
        "transaction should be in InterventionRequired state"
    );

    let actions = harness.recorded_actions().await;
    assert!(actions.iter().any(|action| {
        action.actor == HarnessActorKind::CredentialsProvider
            && action.action == "issue_intent_authorization"
    }));
    assert!(actions.iter().any(|action| {
        action.actor == HarnessActorKind::Merchant && action.action == "update_checkout"
    }));
    assert!(actions.iter().any(|action| {
        action.actor == HarnessActorKind::Shopper && action.action == "begin_intervention"
    }));
}
