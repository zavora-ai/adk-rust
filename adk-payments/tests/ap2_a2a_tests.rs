#![cfg(feature = "ap2-a2a")]

use std::fs;
use std::path::PathBuf;

use adk_payments::protocol::ap2::{
    AP2_A2A_EXTENSION_URI, Ap2A2aArtifact, Ap2A2aMessage, Ap2AgentCardExtension, Ap2Role,
    CartMandate, IntentMandate, PaymentMandate,
};
use serde_json::Value;

fn fixture_a2a(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ap2/a2a").join(path)
}

fn fixture_v01(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ap2/v0.1-alpha").join(path)
}

fn load_json(path: &PathBuf) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

// ---------------------------------------------------------------------------
// AgentCard extension
// ---------------------------------------------------------------------------

#[test]
fn agent_card_extension_deserializes_from_fixture() {
    let json = load_json(&fixture_a2a("agent_card_extension.json"));
    let ext: Ap2AgentCardExtension = serde_json::from_value(json).unwrap();

    assert_eq!(ext.uri, AP2_A2A_EXTENSION_URI);
    assert!(!ext.required);
    assert_eq!(ext.params.roles.len(), 2);
    assert!(ext.params.roles.contains(&Ap2Role::Merchant));
    assert!(ext.params.roles.contains(&Ap2Role::Shopper));
}

#[test]
fn agent_card_extension_validates_successfully() {
    let json = load_json(&fixture_a2a("agent_card_extension.json"));
    let ext: Ap2AgentCardExtension = serde_json::from_value(json).unwrap();
    ext.validate().expect("valid extension should pass validation");
}

#[test]
fn agent_card_extension_with_wrong_uri_fails_validation() {
    let ext = Ap2AgentCardExtension {
        uri: "https://example.com/wrong-uri".to_string(),
        description: None,
        required: false,
        params: adk_payments::protocol::ap2::Ap2RoleMetadata { roles: vec![Ap2Role::Merchant] },
    };
    let result = ext.validate();
    assert!(result.is_err(), "wrong URI should fail validation");
}

// ---------------------------------------------------------------------------
// IntentMandate via A2A Message
// ---------------------------------------------------------------------------

#[test]
fn intent_mandate_message_deserializes_from_fixture() {
    let json = load_json(&fixture_a2a("intent_mandate_message.json"));
    let msg: Ap2A2aMessage = serde_json::from_value(json).unwrap();

    assert_eq!(msg.message_id, "msg-intent-001");
    assert_eq!(msg.context_id.as_deref(), Some("ctx-shopping-001"));
    assert_eq!(msg.task_id.as_deref(), Some("task-buy-shoes"));
    assert_eq!(msg.role, "user");

    let mandate = msg
        .extract_intent_mandate()
        .expect("extraction should not error")
        .expect("mandate should be present");
    assert_eq!(
        mandate.natural_language_description,
        "Buy running shoes under $200 from SportsMart"
    );
    assert!(mandate.user_cart_confirmation_required);
}

#[test]
fn intent_mandate_message_round_trips_through_constructor() {
    let json = load_json(&fixture_v01("intent_mandate.json"));
    let mandate: IntentMandate = serde_json::from_value(json).unwrap();

    let msg = Ap2A2aMessage::intent_mandate(
        "msg-rt-001",
        "user",
        Some("ctx-001".to_string()),
        Some("task-001".to_string()),
        &mandate,
    );

    assert_eq!(msg.message_id, "msg-rt-001");
    assert_eq!(msg.role, "user");

    let extracted = msg
        .extract_intent_mandate()
        .expect("extraction should not error")
        .expect("mandate should be present");
    assert_eq!(extracted, mandate);
}

// ---------------------------------------------------------------------------
// CartMandate via A2A Artifact
// ---------------------------------------------------------------------------

#[test]
fn cart_mandate_artifact_deserializes_from_fixture() {
    let json = load_json(&fixture_a2a("cart_mandate_artifact.json"));
    let artifact: Ap2A2aArtifact = serde_json::from_value(json).unwrap();

    assert_eq!(artifact.artifact_id, "artifact-cart-001");
    assert_eq!(artifact.name.as_deref(), Some("SportsMart Cart Mandate"));

    let mandate = artifact
        .extract_cart_mandate()
        .expect("extraction should not error")
        .expect("mandate should be present");
    assert_eq!(mandate.contents.id, "cart-abc-123");
    assert_eq!(mandate.contents.merchant_name, "SportsMart");
}

#[test]
fn cart_mandate_artifact_round_trips_through_constructor() {
    let json = load_json(&fixture_v01("cart_mandate.json"));
    let mandate: CartMandate = serde_json::from_value(json).unwrap();

    let artifact =
        Ap2A2aArtifact::cart_mandate("artifact-rt-001", Some("Test Cart".to_string()), &mandate);

    assert_eq!(artifact.artifact_id, "artifact-rt-001");
    assert_eq!(artifact.name.as_deref(), Some("Test Cart"));

    let extracted = artifact
        .extract_cart_mandate()
        .expect("extraction should not error")
        .expect("mandate should be present");
    assert_eq!(extracted, mandate);
}

// ---------------------------------------------------------------------------
// PaymentMandate via A2A Message
// ---------------------------------------------------------------------------

#[test]
fn payment_mandate_message_round_trips_through_constructor() {
    let json = load_json(&fixture_v01("payment_mandate.json"));
    let mandate: PaymentMandate = serde_json::from_value(json).unwrap();

    let msg = Ap2A2aMessage::payment_mandate(
        "msg-pm-001",
        "user",
        Some("ctx-001".to_string()),
        Some("task-001".to_string()),
        &mandate,
    );

    assert_eq!(msg.message_id, "msg-pm-001");

    let extracted = msg
        .extract_payment_mandate()
        .expect("extraction should not error")
        .expect("mandate should be present");
    assert_eq!(
        extracted.payment_mandate_contents.payment_mandate_id,
        mandate.payment_mandate_contents.payment_mandate_id
    );
    assert_eq!(
        extracted.payment_mandate_contents.payment_details_id,
        mandate.payment_mandate_contents.payment_details_id
    );
}
