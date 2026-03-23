#![cfg(feature = "acp")]

mod support;

use std::fs;
use std::path::PathBuf;

use adk_payments::ACP_STABLE_BASELINE;
use adk_payments::domain::{
    CommerceMode, OrderSnapshot, OrderState, ProtocolDescriptor, ProtocolExtensions, ReceiptState,
};
use adk_payments::kernel::{MerchantCheckoutService, OrderUpdateCommand};
use adk_payments::protocol::acp::{AcpRouterBuilder, AcpVerificationConfig, IdempotencyMode};
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use jsonschema::validator_for;
use serde_json::{Value, json};
use support::commerce_harness::{
    HarnessActorKind, MultiActorHarness, MultiActorHarnessActors, MultiActorHarnessConfig,
};
use tower::ServiceExt;

fn fixture(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/acp/2026-01-30").join(path)
}

fn load_json(path: &str) -> Value {
    serde_json::from_str(&fs::read_to_string(fixture(path)).unwrap()).unwrap()
}

fn assert_valid(definition: &str, instance: &Value) {
    let mut schema = load_json("json-schema/schema.agentic_checkout.json");
    if definition.starts_with("Delegate") || definition == "Error" {
        schema = load_json("json-schema/schema.delegate_payment.json");
    }
    schema
        .as_object_mut()
        .unwrap()
        .insert("$ref".to_string(), Value::String(format!("#/$defs/{definition}")));
    let validator = validator_for(&schema).unwrap();
    let errors: Vec<_> = validator.iter_errors(instance).map(|error| error.to_string()).collect();
    assert!(errors.is_empty(), "schema validation failed for {definition}: {}", errors.join("; "));
}

fn request(method: Method, uri: &str, body: Value, idempotency_key: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("API-Version", ACP_STABLE_BASELINE)
        .header("Authorization", "Bearer test-token")
        .header("Accept-Language", "en-US");
    if !body.is_null() {
        builder = builder.header("Content-Type", "application/json");
    }
    if let Some(idempotency_key) = idempotency_key {
        builder = builder.header("Idempotency-Key", idempotency_key);
    }
    builder
        .body(Body::from(if body.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(&body).unwrap()
        }))
        .unwrap()
}

async fn response_json(
    app: &mut axum::Router,
    request: Request<Body>,
    expected_status: StatusCode,
) -> Value {
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), expected_status);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn touch_ap2_harness_api() {
    let _ = MultiActorHarnessConfig::ap2_defaults;
    let _ = MultiActorHarness::shopper_context;
    let _ = MultiActorHarness::merchant_context;
    let _ = MultiActorHarness::payment_processor_context;
    let _ = MultiActorHarness::issue_credentials_provider_artifact;
    let _ = |actors: &MultiActorHarnessActors| {
        let _ = &actors.merchant;
        let _ = &actors.payment_processor;
        let _ = &actors.payment_processor_ref;
    };
}

#[tokio::test]
async fn acp_human_present_flow_updates_journal_memory_and_evidence_end_to_end() {
    touch_ap2_harness_api();
    let harness = MultiActorHarness::new(MultiActorHarnessConfig::acp_defaults()).await;

    let mut app = AcpRouterBuilder::new(harness.acp_context_template(CommerceMode::HumanPresent))
        .with_merchant_checkout_service(harness.backend.clone())
        .with_delegated_payment_service(harness.backend.clone())
        .with_verification(
            AcpVerificationConfig::strict().with_idempotency_mode(IdempotencyMode::RequireForPost),
        )
        .build()
        .unwrap();

    let create_response = response_json(
        &mut app,
        request(
            Method::POST,
            "/checkout_sessions",
            json!({
                "currency": "usd",
                "line_items": [{"id": "item_123"}],
                "capabilities": {
                    "interventions": {
                        "supported": ["3ds"],
                        "display_context": "webview",
                        "redirect_context": "in_app",
                        "max_redirects": 1,
                        "max_interaction_depth": 1
                    }
                },
                "fulfillment_details": {
                    "name": "John Doe",
                    "email": "johndoe@example.com",
                    "address": {
                        "name": "John Doe",
                        "line_one": "1234 Chat Road",
                        "city": "San Francisco",
                        "state": "CA",
                        "country": "US",
                        "postal_code": "94131"
                    }
                }
            }),
            Some("idem-create-123"),
        ),
        StatusCode::CREATED,
    )
    .await;
    assert_valid("CheckoutSession", &create_response);
    let checkout_session_id = create_response["id"].as_str().unwrap().to_string();

    let update_response = response_json(
        &mut app,
        request(
            Method::POST,
            &format!("/checkout_sessions/{checkout_session_id}"),
            json!({
                "selected_fulfillment_options": [
                    {
                        "type": "shipping",
                        "option_id": "fulfillment_option_456",
                        "item_ids": ["item_123"]
                    }
                ]
            }),
            Some("idem-update-123"),
        ),
        StatusCode::OK,
    )
    .await;
    assert_valid("CheckoutSession", &update_response);
    assert_eq!(update_response["status"], "ready_for_payment");

    let delegate_response = response_json(
        &mut app,
        request(
            Method::POST,
            "/agentic_commerce/delegate_payment",
            json!({
                "payment_method": {
                    "type": "card",
                    "card_number_type": "fpan",
                    "virtual": false,
                    "number": "4242424242424242",
                    "exp_month": "11",
                    "exp_year": "2026",
                    "name": "Jane Doe",
                    "cvc": "223",
                    "checks_performed": ["avs", "cvv"],
                    "iin": "424242",
                    "display_card_funding_type": "credit",
                    "display_brand": "visa",
                    "display_last4": "4242",
                    "metadata": {"issuing_bank": "temp"}
                },
                "allowance": {
                    "reason": "one_time",
                    "max_amount": 2000,
                    "currency": "usd",
                    "checkout_session_id": checkout_session_id,
                    "merchant_id": "merchant-123",
                    "expires_at": "2026-03-22T12:00:00Z"
                },
                "billing_address": {
                    "name": "Jane Doe",
                    "line_one": "1234 Chat Road",
                    "city": "San Francisco",
                    "state": "CA",
                    "country": "US",
                    "postal_code": "94131"
                },
                "risk_signals": [{"type": "card_testing", "score": 10, "action": "manual_review"}],
                "metadata": {"source": "chatgpt_checkout"}
            }),
            Some("idem-delegate-123"),
        ),
        StatusCode::CREATED,
    )
    .await;
    assert_valid("DelegatePaymentResponse", &delegate_response);

    let complete_response = response_json(
        &mut app,
        request(
            Method::POST,
            &format!("/checkout_sessions/{checkout_session_id}/complete"),
            json!({
                "buyer": {
                    "first_name": "John",
                    "last_name": "Smith",
                    "email": "johnsmith@mail.com",
                    "phone_number": "15552003434"
                },
                "payment_data": {
                    "handler_id": "card_tokenized",
                    "instrument": {
                        "type": "card",
                        "credential": {"type": "spt", "token": "spt_123"}
                    },
                    "billing_address": {
                        "name": "John Smith",
                        "line_one": "1234 Chat Road",
                        "city": "San Francisco",
                        "state": "CA",
                        "country": "US",
                        "postal_code": "94131"
                    }
                }
            }),
            Some("idem-complete-123"),
        ),
        StatusCode::OK,
    )
    .await;
    assert_valid("CheckoutSessionWithOrder", &complete_response);
    assert_eq!(complete_response["status"], "completed");

    harness
        .backend
        .apply_order_update(OrderUpdateCommand {
            context: harness.webhook_context(
                &checkout_session_id,
                CommerceMode::HumanPresent,
                ProtocolDescriptor::acp(ACP_STABLE_BASELINE),
            ),
            order: OrderSnapshot {
                order_id: Some("ord_abc123".to_string()),
                receipt_id: Some("receipt_123".to_string()),
                state: OrderState::Fulfilled,
                receipt_state: ReceiptState::Settled,
                extensions: ProtocolExtensions::default(),
            },
        })
        .await
        .unwrap();

    let get_response = response_json(
        &mut app,
        request(
            Method::GET,
            &format!("/checkout_sessions/{checkout_session_id}"),
            Value::Null,
            None,
        ),
        StatusCode::OK,
    )
    .await;
    assert_valid("CheckoutSession", &get_response);

    let record = harness.transaction(&checkout_session_id).await;
    assert_eq!(record.protocol_refs.acp_delegate_payment_id.as_deref(), Some("vt_01J8Z3WXYZ9ABC"));
    assert_eq!(record.protocol_refs.acp_order_id.as_deref(), Some("ord_abc123"));
    assert_eq!(record.order.as_ref().unwrap().state, OrderState::Fulfilled);

    let state_dump = harness.session_state_dump().await;
    let events_dump = harness.session_events_dump().await;
    assert!(!state_dump.contains("4242424242424242"));
    assert!(!state_dump.contains("\"223\""));
    assert!(!events_dump.contains("4242424242424242"));
    assert!(!events_dump.contains("\"223\""));

    let memory_text = harness.memory_text(&checkout_session_id).await;
    assert!(!memory_text.is_empty());
    assert!(!memory_text.contains("4242424242424242"));
    assert!(!memory_text.contains("223"));
    assert!(memory_text.contains(&checkout_session_id));

    let raw_evidence = record
        .evidence_refs
        .iter()
        .find(|evidence| evidence.artifact_kind == "delegate_payment_request")
        .cloned()
        .expect("delegate payment evidence should exist");
    let stored_evidence = harness.load_evidence(&raw_evidence).await;
    let evidence_text = String::from_utf8(stored_evidence.body).unwrap();
    assert!(evidence_text.contains("4242424242424242"));
    assert!(evidence_text.contains("\"cvc\":\"223\""));

    let actions = harness.recorded_actions().await;
    assert!(actions.iter().any(|action| {
        action.actor == HarnessActorKind::Shopper && action.action == "create_checkout"
    }));
    assert!(actions.iter().any(|action| {
        action.actor == HarnessActorKind::Shopper && action.action == "update_checkout"
    }));
    assert!(actions.iter().any(|action| {
        action.actor == HarnessActorKind::Shopper && action.action == "delegate_payment"
    }));
    assert!(actions.iter().any(|action| {
        action.actor == HarnessActorKind::Shopper && action.action == "complete_checkout"
    }));
    assert!(actions.iter().any(|action| {
        action.actor == HarnessActorKind::Webhook && action.action == "apply_order_update"
    }));
}
