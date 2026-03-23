#![cfg(feature = "acp-experimental")]

use std::fs;
use std::path::PathBuf;

use jsonschema::validator_for;
use serde_json::Value;

fn fixture(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/acp/unreleased").join(path)
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
fn experimental_openapi_documents_expose_expected_paths() {
    let webhook = load_yaml("openapi/openapi.agentic_checkout_webhook.yaml");
    let delegate = load_yaml("openapi/openapi.delegate_authentication.yaml");

    let webhook_paths = webhook["paths"].as_mapping().unwrap();
    assert!(
        webhook_paths
            .contains_key(serde_yaml::Value::from("/agentic_checkout/webhooks/order_events"))
    );

    let delegate_paths = delegate["paths"].as_mapping().unwrap();
    assert!(delegate_paths.contains_key(serde_yaml::Value::from("/delegate_authentication")));
    assert!(delegate_paths.contains_key(serde_yaml::Value::from(
        "/delegate_authentication/{authentication_session_id}/authenticate"
    )));
    assert!(delegate_paths.contains_key(serde_yaml::Value::from(
        "/delegate_authentication/{authentication_session_id}"
    )));
}

#[test]
fn discovery_document_example_validates_against_unreleased_checkout_schema() {
    let schema = load_json("json-schema/schema.agentic_checkout.json");
    let example = schema["$defs"]["DiscoveryResponse"]["example"].clone();
    assert_valid("json-schema/schema.agentic_checkout.json", "DiscoveryResponse", &example);
}

#[test]
fn delegate_authentication_examples_validate_against_unreleased_schema() {
    let examples = load_json("examples/examples.delegate_authentication.json");

    assert_valid(
        "json-schema/schema.delegate_authentication.json",
        "DelegateAuthenticationCreateRequest",
        &examples["create_authentication_session_request_minimal"],
    );
    assert_valid(
        "json-schema/schema.delegate_authentication.json",
        "DelegateAuthenticationCreateRequest",
        &examples["create_authentication_session_request_with_channel_and_acquirer_details"],
    );
    assert_valid(
        "json-schema/schema.delegate_authentication.json",
        "DelegateAuthenticationSession",
        &examples["create_authentication_session_response_fingerprint_required"],
    );
    assert_valid(
        "json-schema/schema.delegate_authentication.json",
        "DelegateAuthenticationAuthenticateRequest",
        &examples["authenticate_request_fingerprint_success"],
    );
    assert_valid(
        "json-schema/schema.delegate_authentication.json",
        "DelegateAuthenticationSession",
        &examples["authenticate_response_challenge_required"],
    );
    let mut retrieve_base = examples["retrieve_session_response_authenticated"].clone();
    retrieve_base.as_object_mut().unwrap().remove("authentication_result");
    assert_valid(
        "json-schema/schema.delegate_authentication.json",
        "DelegateAuthenticationSession",
        &retrieve_base,
    );
    assert_valid(
        "json-schema/schema.delegate_authentication.json",
        "AuthenticationResult",
        &examples["retrieve_session_response_authenticated"]["authentication_result"],
    );
    assert_valid(
        "json-schema/schema.delegate_authentication.json",
        "Error",
        &examples["error_invalid_card"],
    );
}

#[test]
fn webhook_examples_use_the_full_order_schema() {
    let webhook = load_yaml("openapi/openapi.agentic_checkout_webhook.yaml");
    let examples = webhook["paths"]["/agentic_checkout/webhooks/order_events"]["post"]["requestBody"]
        ["content"]["application/json"]["examples"]
        .as_mapping()
        .unwrap();

    for example_name in ["order_created", "order_shipped", "order_with_refund"] {
        let yaml_value = &examples[serde_yaml::Value::from(example_name)]["value"];
        let value = serde_json::to_value(yaml_value).unwrap();
        assert_eq!(value["data"]["type"], "order");
        assert_valid("json-schema/schema.agentic_checkout.json", "Order", &value["data"]);
    }
}
