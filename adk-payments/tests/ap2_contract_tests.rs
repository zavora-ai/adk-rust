#![cfg(feature = "ap2")]

use std::fs;
use std::path::PathBuf;

use adk_payments::domain::{
    Cart, CartLine, CommerceActor, CommerceActorRole, CommerceMode, MerchantRef, Money,
    ProtocolExtensions, TransactionId, TransactionRecord, TransactionState,
};
use adk_payments::protocol::ap2::{
    Ap2Role, Ap2RoleMetadata, CartMandate, IntentMandate, PaymentMandate, PaymentReceipt,
};

#[cfg(feature = "ap2-a2a")]
use adk_payments::protocol::ap2::{Ap2A2aArtifact, Ap2A2aMessage, Ap2AgentCardExtension};

#[cfg(feature = "ap2-mcp")]
use adk_payments::protocol::ap2::{Ap2McpReceiptStatus, Ap2ReceiptStatusKind};

use chrono::{TimeZone, Utc};
use serde_json::Value;

fn fixture(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ap2").join(path)
}

fn load_json(path: &str) -> Value {
    serde_json::from_str(&fs::read_to_string(fixture(path)).unwrap()).unwrap()
}

fn sample_record() -> TransactionRecord {
    TransactionRecord::new(
        TransactionId::from("tx-ap2-contract"),
        CommerceActor {
            actor_id: "shopper-agent".to_string(),
            role: CommerceActorRole::Shopper,
            display_name: Some("shopper".to_string()),
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
                merchant_sku: None,
                title: "Red Running Shoes".to_string(),
                quantity: 1,
                unit_price: Money::new("USD", 12_000, 2),
                total_price: Money::new("USD", 12_000, 2),
                product_class: None,
                extensions: ProtocolExtensions::default(),
            }],
            subtotal: Some(Money::new("USD", 12_000, 2)),
            adjustments: Vec::new(),
            total: Money::new("USD", 12_500, 2),
            affiliate_attribution: None,
            extensions: ProtocolExtensions::default(),
        },
        Utc.with_ymd_and_hms(2026, 3, 22, 10, 0, 0).unwrap(),
    )
}

#[test]
fn parses_ap2_fixture_payloads() {
    let intent: IntentMandate =
        serde_json::from_value(load_json("v0.1-alpha/intent_mandate.json")).unwrap();
    let cart: CartMandate =
        serde_json::from_value(load_json("v0.1-alpha/cart_mandate.json")).unwrap();
    let payment: PaymentMandate =
        serde_json::from_value(load_json("v0.1-alpha/payment_mandate.json")).unwrap();
    let receipt: PaymentReceipt =
        serde_json::from_value(load_json("v0.1-alpha/payment_receipt.json")).unwrap();

    assert!(intent.user_cart_confirmation_required);
    assert_eq!(intent.merchants.as_ref().unwrap(), &vec!["Merchant Example".to_string()]);
    assert_eq!(cart.contents.id, "cart_shoes_123");
    assert_eq!(payment.payment_mandate_contents.payment_mandate_id, "pm_12345");
    assert_eq!(receipt.payment_id, "pay_12345");
}

#[cfg(feature = "ap2-a2a")]
#[test]
fn validates_a2a_extension_and_extracts_mandates() {
    let extension: Ap2AgentCardExtension =
        serde_json::from_value(load_json("a2a/agent_card_extension.json")).unwrap();
    extension.validate().unwrap();
    assert_eq!(
        extension.params,
        Ap2RoleMetadata { roles: vec![Ap2Role::Merchant, Ap2Role::Shopper] }
    );

    let intent_message: Ap2A2aMessage =
        serde_json::from_value(load_json("a2a/intent_message.json")).unwrap();
    let cart_artifact: Ap2A2aArtifact =
        serde_json::from_value(load_json("a2a/cart_artifact.json")).unwrap();
    let payment_message: Ap2A2aMessage =
        serde_json::from_value(load_json("a2a/payment_message.json")).unwrap();

    assert_eq!(
        intent_message.extract_intent_mandate().unwrap().unwrap().intent_expiry,
        "2026-04-01T12:00:00Z"
    );
    assert_eq!(
        cart_artifact.extract_cart_mandate().unwrap().unwrap().contents.id,
        "cart_shoes_123"
    );
    assert_eq!(
        payment_message
            .extract_payment_mandate()
            .unwrap()
            .unwrap()
            .payment_mandate_contents
            .payment_mandate_id,
        "pm_12345"
    );
}

#[cfg(feature = "ap2-mcp")]
#[test]
fn mcp_receipt_view_omits_sensitive_payment_details() {
    let mut record = sample_record();
    record.state = TransactionState::Completed;
    record.protocol_refs.ap2_payment_receipt_id = Some("pay_12345".to_string());
    let receipt: PaymentReceipt =
        serde_json::from_value(load_json("v0.1-alpha/payment_receipt.json")).unwrap();

    let view = Ap2McpReceiptStatus::from_receipt(&record, &receipt);
    let encoded = serde_json::to_string(&view).unwrap();

    assert_eq!(view.status, Ap2ReceiptStatusKind::Success);
    assert!(encoded.contains("CARD"));
    assert!(!encoded.contains("tok_123"));
}
