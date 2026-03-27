#![cfg(feature = "acp")]

use std::fs;
use std::path::PathBuf;

use jsonschema::validator_for;
use serde_json::Value;

fn fixture(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/acp/2026-01-30").join(path)
}

fn load_json(path: &str) -> Value {
    serde_json::from_str(&fs::read_to_string(fixture(path)).unwrap()).unwrap()
}

fn load_yaml(path: &str) -> serde_yaml::Value {
    serde_yaml::from_str(&fs::read_to_string(fixture(path)).unwrap()).unwrap()
}

fn assert_valid(schema_path: &str, definition: &str, instance: &Value) {
    let mut schema = load_json(schema_path);
    schema
        .as_object_mut()
        .unwrap()
        .insert("$ref".to_string(), Value::String(format!("#/$defs/{definition}")));

    let validator = validator_for(&schema).unwrap();
    let errors: Vec<_> = validator.iter_errors(instance).map(|error| error.to_string()).collect();
    assert!(errors.is_empty(), "fixture failed validation for {definition}: {}", errors.join("; "));
}

#[test]
fn stable_openapi_documents_expose_expected_checkout_and_delegate_paths() {
    let checkout = load_yaml("openapi/openapi.agentic_checkout.yaml");
    let delegate = load_yaml("openapi/openapi.delegate_payment.yaml");

    let checkout_paths = checkout["paths"].as_mapping().unwrap();
    assert!(checkout_paths.contains_key(serde_yaml::Value::from("/checkout_sessions")));
    assert!(
        checkout_paths
            .contains_key(serde_yaml::Value::from("/checkout_sessions/{checkout_session_id}"))
    );
    assert!(checkout_paths.contains_key(serde_yaml::Value::from(
        "/checkout_sessions/{checkout_session_id}/complete"
    )));
    assert!(
        checkout_paths.contains_key(serde_yaml::Value::from(
            "/checkout_sessions/{checkout_session_id}/cancel"
        ))
    );

    let delegate_paths = delegate["paths"].as_mapping().unwrap();
    assert!(
        delegate_paths.contains_key(serde_yaml::Value::from("/agentic_commerce/delegate_payment"))
    );
}

#[test]
fn checkout_examples_validate_against_stable_checkout_schema() {
    let examples = load_json("examples/examples.agentic_checkout.json");

    assert_valid(
        "json-schema/schema.agentic_checkout.json",
        "CheckoutSessionCreateRequest",
        &examples["create_checkout_session_request"],
    );
    assert_valid(
        "json-schema/schema.agentic_checkout.json",
        "CheckoutSession",
        &examples["create_checkout_session_response"],
    );
    assert_valid(
        "json-schema/schema.agentic_checkout.json",
        "CheckoutSessionUpdateRequest",
        &examples["update_checkout_session_request"],
    );
    assert_valid(
        "json-schema/schema.agentic_checkout.json",
        "CheckoutSession",
        &examples["update_checkout_session_response"],
    );
    assert_valid(
        "json-schema/schema.agentic_checkout.json",
        "CheckoutSessionCompleteRequest",
        &examples["complete_checkout_session_request"],
    );
    assert_valid(
        "json-schema/schema.agentic_checkout.json",
        "CheckoutSessionWithOrder",
        &examples["complete_checkout_session_response"],
    );
    assert_valid(
        "json-schema/schema.agentic_checkout.json",
        "CancelSessionRequest",
        &examples["cancel_checkout_session_request"],
    );
    assert_valid(
        "json-schema/schema.agentic_checkout.json",
        "CheckoutSession",
        &examples["cancel_checkout_session_response"],
    );
    assert_valid(
        "json-schema/schema.agentic_checkout.json",
        "CheckoutSession",
        &examples["get_checkout_session_response"],
    );
}

#[test]
fn delegate_payment_examples_validate_against_stable_delegate_payment_schema() {
    let examples = load_json("examples/examples.delegate_payment.json");

    assert_valid(
        "json-schema/schema.delegate_payment.json",
        "DelegatePaymentRequest",
        &examples["delegate_payment_request"],
    );
    assert_valid(
        "json-schema/schema.delegate_payment.json",
        "DelegatePaymentResponse",
        &examples["delegate_payment_success_response"],
    );
    assert_valid(
        "json-schema/schema.delegate_payment.json",
        "Error",
        &examples["delegate_payment_error_invalid_card"],
    );
    assert_valid(
        "json-schema/schema.delegate_payment.json",
        "Error",
        &examples["delegate_payment_error_idempotency_conflict"],
    );
    assert_valid(
        "json-schema/schema.delegate_payment.json",
        "Error",
        &examples["delegate_payment_error_rate_limit"],
    );
}

#[test]
fn simple_order_fixture_validates_against_checkout_order_schema() {
    let mut order = load_json("orders/simple-order.json");
    order.as_object_mut().unwrap().remove("$schema");
    order.as_object_mut().unwrap().remove("_description");
    assert_valid("json-schema/schema.agentic_checkout.json", "Order", &order);
}
