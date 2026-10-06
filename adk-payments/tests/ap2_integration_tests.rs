#![cfg(feature = "ap2")]

mod support;

use std::sync::Arc;

use adk_core::{AdkError, ErrorCategory, ErrorComponent};
use adk_payments::AP2_ALPHA_BASELINE;
use adk_payments::domain::{
    Cart, CartLine, CommerceMode, InterventionKind, InterventionStatus, MAX_MONEY_SCALE, Money,
    MoneyError, OrderState, ProtocolDescriptor, ProtocolExtensions, ReceiptState, TransactionState,
    TransactionStateTag,
};
use adk_payments::kernel::CommerceContext;
use adk_payments::protocol::ap2::{
    Ap2Adapter, Ap2Error, AuthorizationArtifact, CRYPTOGRAPHICALLY_VERIFIED_CLAIM, CartMandate,
    IntentMandate, MerchantAuthorizationVerifier, PaymentMandate, PaymentReceipt,
    UserAuthorizationVerifier, VERIFIED_MERCHANT_NAME_CLAIM, VerifiedAuthorization,
};
use async_trait::async_trait;
use chrono::{Duration, Utc};
use hmac::{Hmac, Mac};
use serde::Serialize;
use serde_json::{Map, Value, json};
use sha2::Sha256;
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
    // Flow coverage only; signature verification is covered by the HMAC verifier tests below.
    let adapter = Ap2Adapter::new(
        harness.backend.clone(),
        harness.backend.clone(),
        harness.backend.clone(),
        harness.backend.clone(),
    )
    .allow_unverified_authorizations();

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
    .allow_unverified_authorizations()
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

type HmacSha256 = Hmac<Sha256>;

const MERCHANT_KEY: &[u8] = b"merchant-signing-key";
const USER_KEY: &[u8] = b"user-signing-key";

fn forged_authorization() -> AdkError {
    AdkError::new(
        ErrorComponent::Auth,
        ErrorCategory::Unauthorized,
        "payments.ap2.test.forged_authorization",
        "authorization signature does not match the mandate",
    )
}

fn hmac_hex(key: &[u8], payload: &impl Serialize) -> String {
    let mut mac = HmacSha256::new_from_slice(key).unwrap();
    mac.update(&serde_json::to_vec(payload).unwrap());
    hex::encode(mac.finalize().into_bytes())
}

fn verify_hmac(key: &[u8], payload: &impl Serialize, signature: &str) -> adk_core::Result<()> {
    let signature = hex::decode(signature).map_err(|_| forged_authorization())?;
    let mut mac = HmacSha256::new_from_slice(key).unwrap();
    mac.update(&serde_json::to_vec(payload).unwrap());
    // `verify_slice` compares in constant time.
    mac.verify_slice(&signature).map_err(|_| forged_authorization())
}

/// Accepts carts whose authorization is an HMAC of `contents` under the merchant key.
struct HmacMerchantVerifier {
    merchant_name: String,
}

#[async_trait]
impl MerchantAuthorizationVerifier for HmacMerchantVerifier {
    async fn verify_cart_authorization(
        &self,
        mandate: &CartMandate,
        artifact: &AuthorizationArtifact,
    ) -> adk_core::Result<VerifiedAuthorization> {
        verify_hmac(MERCHANT_KEY, &mandate.contents, &artifact.value)?;
        let mut claims = Map::new();
        claims.insert(VERIFIED_MERCHANT_NAME_CLAIM.to_string(), json!(self.merchant_name));
        Ok(VerifiedAuthorization::new("merchant_authorization", claims))
    }
}

/// Accepts intents and payment mandates signed with the user key.
struct HmacUserVerifier;

#[async_trait]
impl UserAuthorizationVerifier for HmacUserVerifier {
    async fn verify_intent_authorization(
        &self,
        mandate: &IntentMandate,
        artifact: &AuthorizationArtifact,
    ) -> adk_core::Result<VerifiedAuthorization> {
        verify_hmac(USER_KEY, mandate, &artifact.value)?;
        Ok(VerifiedAuthorization::new("intent_authorization", Map::new()))
    }

    async fn verify_payment_authorization(
        &self,
        mandate: &PaymentMandate,
        artifact: &AuthorizationArtifact,
    ) -> adk_core::Result<VerifiedAuthorization> {
        verify_hmac(USER_KEY, &mandate.payment_mandate_contents, &artifact.value)?;
        Ok(VerifiedAuthorization::new("user_authorization", Map::new()))
    }
}

fn signed_cart_mandate(details_id: &str) -> CartMandate {
    let mut cart = make_cart_mandate(details_id);
    cart.merchant_authorization = Some(hmac_hex(MERCHANT_KEY, &cart.contents));
    cart
}

fn signed_payment_mandate(details_id: &str) -> PaymentMandate {
    let mut payment = make_payment_mandate(details_id, None);
    payment.user_authorization = Some(hmac_hex(USER_KEY, &payment.payment_mandate_contents));
    payment
}

fn cart_with_amounts(cart_id: &str, currency: &str, items: &[Value], total: Value) -> CartMandate {
    let display_items: Vec<Value> = items
        .iter()
        .enumerate()
        .map(|(index, value)| {
            json!({"label": format!("item-{index}"), "amount": {"currency": currency, "value": value}})
        })
        .collect();
    serde_json::from_value(json!({
        "contents": {
            "id": cart_id,
            "user_cart_confirmation_required": false,
            "payment_request": {
                "method_data": [{"supported_methods": "CARD"}],
                "details": {
                    "id": cart_id,
                    "display_items": display_items,
                    "total": {"label": "Total", "amount": {"currency": currency, "value": total}}
                }
            },
            "cart_expiry": future_expiry(),
            "merchant_name": "AP2 Merchant"
        },
        "merchant_authorization": "unverified-merchant-signature"
    }))
    .expect("cart mandate JSON should be valid")
}

fn assert_ap2_error(error: AdkError, expected: Ap2Error) {
    let expected = AdkError::from(expected);
    assert_eq!((error.code, error.message), (expected.code, expected.message));
}

async fn execute_count(harness: &MultiActorHarness, transaction_id: &str) -> usize {
    harness
        .recorded_actions()
        .await
        .iter()
        .filter(|action| {
            action.transaction_id == transaction_id && action.action == "execute_payment"
        })
        .count()
}

fn ap2_contexts(
    harness: &MultiActorHarness,
    transaction_id: &str,
    mode: CommerceMode,
) -> (CommerceContext, CommerceContext) {
    let protocol = ProtocolDescriptor::ap2(AP2_ALPHA_BASELINE);
    (
        harness.merchant_context(transaction_id, mode, protocol.clone()),
        harness.shopper_context(transaction_id, mode, protocol),
    )
}

#[tokio::test]
async fn ap2_adapter_without_verifiers_rejects_every_authorization() {
    touch_acp_harness_api();
    let harness = MultiActorHarness::new(MultiActorHarnessConfig::ap2_defaults()).await;
    let backend = harness.backend.clone();
    let unconfigured =
        Ap2Adapter::new(backend.clone(), backend.clone(), backend.clone(), backend.clone());
    let (merchant, shopper) = ap2_contexts(&harness, "no-verifier-001", CommerceMode::HumanPresent);

    let error = unconfigured
        .submit_cart_mandate(merchant.clone(), make_cart_mandate("no-verifier-cart"))
        .await
        .unwrap_err();
    assert_ap2_error(
        error,
        Ap2Error::AuthorizationVerifierNotConfigured {
            artifact_kind: "merchant_authorization".to_string(),
        },
    );

    let (_, hnp_shopper) =
        ap2_contexts(&harness, "no-verifier-intent", CommerceMode::HumanNotPresent);
    let error = unconfigured
        .submit_intent_mandate(
            hnp_shopper,
            IntentMandate {
                user_cart_confirmation_required: false,
                natural_language_description: "Buy shoes".to_string(),
                merchants: Some(vec!["AP2 Merchant".to_string()]),
                skus: None,
                requires_refundability: false,
                intent_expiry: future_expiry(),
            },
            Some(AuthorizationArtifact::new("user_intent_authorization", "x", "text/plain")),
        )
        .await
        .unwrap_err();
    assert_ap2_error(
        error,
        Ap2Error::AuthorizationVerifierNotConfigured {
            artifact_kind: "intent_authorization".to_string(),
        },
    );

    // A configured merchant verifier does not stand in for the missing user verifier.
    let merchant_only = Ap2Adapter::new(backend.clone(), backend.clone(), backend.clone(), backend)
        .with_merchant_authorization_verifier(Arc::new(HmacMerchantVerifier {
            merchant_name: "AP2 Merchant".to_string(),
        }));
    merchant_only
        .submit_cart_mandate(merchant, signed_cart_mandate("no-verifier-cart"))
        .await
        .expect("signed cart should pass the merchant verifier");
    let error = merchant_only
        .submit_payment_mandate(
            shopper,
            make_payment_mandate("no-verifier-cart", Some("x".to_string())),
        )
        .await
        .unwrap_err();
    assert_ap2_error(
        error,
        Ap2Error::AuthorizationVerifierNotConfigured {
            artifact_kind: "user_authorization".to_string(),
        },
    );
    assert_eq!(execute_count(&harness, "no-verifier-001").await, 0);
}

#[tokio::test]
async fn ap2_verifiers_reject_forged_authorizations_and_accept_signed_ones() {
    touch_acp_harness_api();
    let harness = MultiActorHarness::new(MultiActorHarnessConfig::ap2_defaults()).await;
    let backend = harness.backend.clone();
    let adapter = Ap2Adapter::new(backend.clone(), backend.clone(), backend.clone(), backend)
        .with_merchant_authorization_verifier(Arc::new(HmacMerchantVerifier {
            merchant_name: "AP2 Merchant".to_string(),
        }))
        .with_user_authorization_verifier(Arc::new(HmacUserVerifier));
    let tx_id = "verified-001";
    let (merchant, shopper) = ap2_contexts(&harness, tx_id, CommerceMode::HumanPresent);

    let mut forged_cart = make_cart_mandate("verified-cart");
    forged_cart.merchant_authorization = Some("x".to_string());
    let error = adapter.submit_cart_mandate(merchant.clone(), forged_cart).await.unwrap_err();
    assert_eq!(error.code, "payments.ap2.test.forged_authorization");

    let mut impostor = make_cart_mandate("verified-cart");
    impostor.contents.merchant_name = "Impostor Shop".to_string();
    impostor.merchant_authorization = Some(hmac_hex(MERCHANT_KEY, &impostor.contents));
    let error = adapter.submit_cart_mandate(merchant.clone(), impostor).await.unwrap_err();
    assert_ap2_error(
        error,
        Ap2Error::MerchantIdentityMismatch {
            claimed: "Impostor Shop".to_string(),
            verified: "AP2 Merchant".to_string(),
        },
    );

    adapter
        .submit_cart_mandate(merchant, signed_cart_mandate("verified-cart"))
        .await
        .expect("signed cart should be accepted");

    let error = adapter
        .submit_payment_mandate(
            shopper.clone(),
            make_payment_mandate("verified-cart", Some("x".to_string())),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "payments.ap2.test.forged_authorization");
    assert_eq!(execute_count(&harness, tx_id).await, 0);

    let result = adapter
        .submit_payment_mandate(shopper, signed_payment_mandate("verified-cart"))
        .await
        .expect("signed payment mandate should execute");
    assert_eq!(result.outcome, adk_payments::kernel::PaymentExecutionOutcome::Completed);
    assert_eq!(execute_count(&harness, tx_id).await, 1);
}

#[tokio::test]
async fn ap2_unverified_opt_in_accepts_any_non_empty_authorization() {
    touch_acp_harness_api();
    let harness = MultiActorHarness::new(MultiActorHarnessConfig::ap2_defaults()).await;
    let backend = harness.backend.clone();
    let adapter = Ap2Adapter::new(backend.clone(), backend.clone(), backend.clone(), backend)
        .allow_unverified_authorizations();
    let tx_id = "unverified-001";
    let (merchant, shopper) = ap2_contexts(&harness, tx_id, CommerceMode::HumanPresent);

    adapter.submit_cart_mandate(merchant, make_cart_mandate("unverified-cart")).await.unwrap();
    let result = adapter
        .submit_payment_mandate(
            shopper,
            make_payment_mandate("unverified-cart", Some("x".to_string())),
        )
        .await
        .expect("the development opt-in accepts any non-empty authorization");

    let verification = result
        .transaction
        .extensions
        .as_slice()
        .iter()
        .find_map(|envelope| envelope.fields.get("user_authorization_verification"))
        .expect("payment envelope should record the verification");
    assert_eq!(verification["claims"][CRYPTOGRAPHICALLY_VERIFIED_CLAIM], json!(false));
}

#[tokio::test]
async fn ap2_payment_mandates_are_single_use() {
    touch_acp_harness_api();
    let harness = MultiActorHarness::new(MultiActorHarnessConfig::ap2_defaults()).await;
    let backend = harness.backend.clone();
    let adapter = Ap2Adapter::new(backend.clone(), backend.clone(), backend.clone(), backend)
        .allow_unverified_authorizations();
    let tx_id = "replay-001";
    let details_id = "replay-cart";
    let (merchant, shopper) = ap2_contexts(&harness, tx_id, CommerceMode::HumanPresent);
    let payment = make_payment_mandate(details_id, Some("user-signed".to_string()));

    adapter.submit_cart_mandate(merchant, make_cart_mandate(details_id)).await.unwrap();
    adapter.submit_payment_mandate(shopper.clone(), payment.clone()).await.unwrap();

    let error = adapter.submit_payment_mandate(shopper.clone(), payment.clone()).await.unwrap_err();
    assert_ap2_error(
        error,
        Ap2Error::PaymentMandateReplayed {
            payment_mandate_id: format!("pm-{details_id}"),
            transaction_id: tx_id.to_string(),
        },
    );

    let mut second = payment.clone();
    second.payment_mandate_contents.payment_mandate_id = "pm-second".to_string();
    let error = adapter.submit_payment_mandate(shopper, second).await.unwrap_err();
    assert_ap2_error(
        error,
        Ap2Error::TransactionAlreadyPaid {
            transaction_id: tx_id.to_string(),
            executed_payment_mandate_id: format!("pm-{details_id}"),
        },
    );
    assert_eq!(execute_count(&harness, tx_id).await, 1);

    let processor = harness.payment_processor_context(
        tx_id,
        CommerceMode::HumanPresent,
        ProtocolDescriptor::ap2(AP2_ALPHA_BASELINE),
    );
    let error = adapter
        .apply_payment_receipt(processor, make_success_receipt("pm-other"))
        .await
        .unwrap_err();
    assert_ap2_error(
        error,
        Ap2Error::PaymentMandateMismatch {
            payment_mandate_id: "pm-other".to_string(),
            field: "payment_receipt.payment_mandate_id".to_string(),
        },
    );
    let record = harness.transaction(tx_id).await;
    assert_eq!(record.protocol_refs.ap2_payment_mandate_id, Some(format!("pm-{details_id}")));

    // The same signed cart and mandate replayed into a fresh transaction.
    let other_tx = "replay-002";
    let (other_merchant, other_shopper) =
        ap2_contexts(&harness, other_tx, CommerceMode::HumanPresent);
    adapter.submit_cart_mandate(other_merchant, make_cart_mandate(details_id)).await.unwrap();
    let error = adapter.submit_payment_mandate(other_shopper, payment).await.unwrap_err();
    assert_ap2_error(
        error,
        Ap2Error::PaymentMandateReplayed {
            payment_mandate_id: format!("pm-{details_id}"),
            transaction_id: tx_id.to_string(),
        },
    );
    assert_eq!(execute_count(&harness, other_tx).await, 0);
}

#[tokio::test]
async fn ap2_refundability_alone_is_not_an_authority_constraint() {
    touch_acp_harness_api();
    let harness = MultiActorHarness::new(MultiActorHarnessConfig::ap2_defaults()).await;
    let backend = harness.backend.clone();
    let adapter = Ap2Adapter::new(backend.clone(), backend.clone(), backend.clone(), backend)
        .allow_unverified_authorizations();
    let (_, shopper) = ap2_contexts(&harness, "refund-only-001", CommerceMode::HumanNotPresent);

    let error = adapter
        .submit_intent_mandate(
            shopper,
            IntentMandate {
                user_cart_confirmation_required: false,
                natural_language_description: "Buy anything refundable".to_string(),
                merchants: None,
                skus: None,
                requires_refundability: true,
                intent_expiry: future_expiry(),
            },
            Some(AuthorizationArtifact::new("user_intent_authorization", "signed", "text/plain")),
        )
        .await
        .unwrap_err();
    assert_ap2_error(
        error,
        Ap2Error::MissingAuthorityConstraints { transaction_id: "refund-only-001".to_string() },
    );
}

#[tokio::test]
async fn ap2_cart_amounts_are_normalised_to_one_scale() {
    touch_acp_harness_api();
    let harness = MultiActorHarness::new(MultiActorHarnessConfig::ap2_defaults()).await;
    let backend = harness.backend.clone();
    let adapter = Ap2Adapter::new(backend.clone(), backend.clone(), backend.clone(), backend)
        .allow_unverified_authorizations();
    let (merchant, _) = ap2_contexts(&harness, "scale-001", CommerceMode::HumanPresent);

    let cart = cart_with_amounts(
        "scale-cart",
        "USD",
        &[json!(1.125), json!(2.5), json!(-0.5)],
        json!(3.125),
    );
    let record = adapter.submit_cart_mandate(merchant, cart).await.unwrap();
    let line = |index: usize, amount_minor: i64| CartLine {
        line_id: format!("scale-cart:{index}"),
        merchant_sku: None,
        title: format!("item-{index}"),
        quantity: 1,
        unit_price: Money::new("USD", amount_minor, 3),
        total_price: Money::new("USD", amount_minor, 3),
        product_class: None,
        extensions: ProtocolExtensions::default(),
    };
    assert_eq!(
        record.cart,
        Cart {
            cart_id: Some("scale-cart".to_string()),
            lines: vec![line(0, 1_125), line(1, 2_500), line(2, -500)],
            subtotal: Some(Money::new("USD", 3_125, 3)),
            adjustments: Vec::new(),
            total: Money::new("USD", 3_125, 3),
            affiliate_attribution: None,
            extensions: ProtocolExtensions::default(),
        }
    );

    let (yen_merchant, _) = ap2_contexts(&harness, "scale-002", CommerceMode::HumanPresent);
    let record = adapter
        .submit_cart_mandate(
            yen_merchant,
            cart_with_amounts("yen-cart", "JPY", &[json!(1200)], json!(1200)),
        )
        .await
        .unwrap();
    assert_eq!(record.cart.total, Money::new("JPY", 1_200, 0));
    assert!(record.cart.adjustments.is_empty());

    let (bad_merchant, _) = ap2_contexts(&harness, "scale-003", CommerceMode::HumanPresent);
    let error = adapter
        .submit_cart_mandate(
            bad_merchant.clone(),
            cart_with_amounts("tiny-cart", "USD", &[json!(1e-19)], json!(1)),
        )
        .await
        .unwrap_err();
    assert_ap2_error(
        error,
        Ap2Error::InvalidAmount(MoneyError::PrecisionTooHigh {
            value: "1e-19".to_string(),
            scale: 19,
            max_scale: MAX_MONEY_SCALE,
        }),
    );

    let mut mixed = cart_with_amounts("mixed-cart", "USD", &[json!(1)], json!(1));
    mixed.contents.payment_request.details.display_items[0].amount.currency = "EUR".to_string();
    let error = adapter.submit_cart_mandate(bad_merchant, mixed).await.unwrap_err();
    assert_ap2_error(
        error,
        Ap2Error::InvalidAmount(MoneyError::CurrencyMismatch {
            expected: "USD".to_string(),
            found: "EUR".to_string(),
        }),
    );
    assert!(
        !harness.recorded_actions().await.iter().any(|action| action.transaction_id == "scale-003"),
        "rejected carts must not reach the checkout backend"
    );
}
