//! S9 — agent-to-agent business (spec 3.5 §5 S9, §7.4, §7.5, §7.14, A-I7, A-I8, A-I25).
//!
//! A buyer agent, signed with Web Bot Auth, follows the shop's contract to its manifest,
//! takes a quote, places an order (non-idempotent) and pays for it; a second, large order
//! exceeds the owner's threshold. Every request is sent three times: once, then retried
//! twice.
//!
//! The owner's rail here is a sandbox desk: it settles in a sandbox ledger and issues a
//! receipt signed by the edge's `receipt` key. AP2 mandates cannot travel through the edge on
//! main (see the gaps below), so the rail is not AP2.
//!
//! Pass condition: one booking and one payment per request; the large one answers 202,
//! completes after the owner approves on the phone; consent and signed receipts issued;
//! retries answered from the action ledger.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_awp::signing::jws::{DocumentSigner, Ed25519DocumentSigner, sign, verify_value};
use adk_awp::v2::capability::{CapabilityCall, CapabilityError};
use adk_core::Result as AdkResult;
use adk_payments::wallet::{ReceiptDetails, ReceiptSigner, issue_receipt};
use adk_website::edge::handlers::PaymentDesk;
use anyhow::Result;
use awp_types::{
    ContentId, ContentItem, ContentKind, ContentState, ErrorCode, Markdown, Proof, SiteKind,
};
use axum::http::{StatusCode, header};
use serde_json::{Value, json};

use crate::checks::Checks;
use crate::fixture::edge::{EdgeOptions, edge_with, site_keys};
use crate::fixture::*;
use crate::live::Mode;

const DOMAIN: &str = "juniper.example";

/// Signs receipts with the edge's delegated `receipt` key.
struct EdgeReceiptKey(Arc<Ed25519DocumentSigner>);

#[async_trait::async_trait]
impl ReceiptSigner for EdgeReceiptKey {
    async fn sign(&self, document: &Value) -> AdkResult<Proof> {
        sign(document, self.0.as_ref()).await.map_err(|e| adk_core::AdkError::tool(e.to_string()))
    }
}

/// The owner's sandbox rail: settles each order once and returns a signed receipt.
struct SandboxDesk {
    key: Arc<Ed25519DocumentSigner>,
    settled: AtomicUsize,
}

#[async_trait::async_trait]
impl PaymentDesk for SandboxDesk {
    async fn take(&self, call: &CapabilityCall) -> Result<Value, CapabilityError> {
        let order = call.input["orderId"].as_str().unwrap_or_default();
        self.settled.fetch_add(1, Ordering::SeqCst);
        let mut details =
            ReceiptDetails::new(&call.capability, order, 1, "KES").with_rail("sandbox");
        if let Some(requester) = &call.requester {
            details = details.for_requester(requester.clone());
        }
        let receipt = issue_receipt(&EdgeReceiptKey(self.key.clone()), details, chrono::Utc::now())
            .await
            .map_err(|e| CapabilityError::Refused {
                code: ErrorCode::PaymentPolicyRejected,
                message: e.to_string(),
            })?;
        Ok(json!({ "status": "paid", "orderId": order, "receipt": receipt }))
    }
}

fn product(sku: &str, title: &str, price: u64) -> ContentItem {
    ContentItem {
        id: ContentId(format!("products/{sku}/en")),
        kind: ContentKind::Product,
        slug: sku.into(),
        language: "en".parse().expect("a language"),
        fields: [
            ("title".to_string(), json!(title)),
            ("summary".to_string(), json!(title)),
            ("sku".to_string(), json!(sku)),
            ("price".to_string(), json!(price)),
            ("currency".to_string(), json!("KES")),
            ("goal".to_string(), json!("sales")),
        ]
        .into_iter()
        .collect(),
        volatile: Vec::new(),
        body: Some(Markdown(format!("{title}, baked this morning.\n"))),
        media: Vec::new(),
        sources: Vec::new(),
        schedule: None,
        state: ContentState::Draft,
    }
}

/// Runs S9.
pub async fn run(_mode: Mode) -> Result<Checks> {
    let mut checks = Checks::default();
    let mut site = Site::new(SiteKind::Ecommerce, "Juniper Bakery", DOMAIN, &["en"]).await?;
    site.launch_signed(
        "Juniper Bakery",
        "KES",
        vec![product("rye-loaf", "Rye loaf", 450), product("wedding-cake", "Wedding cake", 25_000)],
    )
    .await?;
    let keys = site_keys(DOMAIN)?;
    let desk = Arc::new(SandboxDesk { key: keys.edge.clone(), settled: AtomicUsize::new(0) });
    let shop = edge_with(
        &site,
        DOMAIN,
        EdgeOptions { payments: Some(desk.clone()), ..EdgeOptions::default() },
    )
    .await?;

    // The buyer follows the contract to the manifest.
    let (_, _, contract) = shop.send(shop.get("/.well-known/awp.json")).await;
    let manifest_url = contract["surfaces"]["manifest"].as_str().unwrap_or_default().to_string();
    let manifest_path =
        url::Url::parse(&manifest_url).map(|u| u.path().to_string()).unwrap_or_default();
    let (_, _, manifest) = shop.send(shop.signed(shop.get(&manifest_path))).await;
    checks.check(
        "the buyer follows the signed contract to the manifest, as a known agent",
        manifest_path == "/awp/v2/manifest" && manifest["tier"] == "known",
        format!("{manifest_url}: {:?}", crate::fixture::edge::EdgeSite::names(&manifest)),
    );

    // A quote, then an order against it, sent three times: one order.
    let quote_body = json!({ "items": [{ "sku": "rye-loaf", "quantity": 3 }] });
    let (_, _, quote) =
        shop.send(shop.signed(shop.call("get_quote", "q-1", None, &quote_body))).await;
    let payment = shop.consent("payment_data").await?;
    let order_body = json!({ "quoteId": quote["id"] });
    let mut orders = Vec::new();
    for _ in 0..3 {
        let (status, _, order) = shop
            .send(shop.signed(shop.call("place_order", "order-1", Some(&payment), &order_body)))
            .await;
        orders.push((
            status,
            order["id"].as_str().unwrap_or_default().to_string(),
            order["quote"]["total"].clone(),
        ));
    }
    checks.check(
        "one booking per request: the retries are answered from the record",
        orders.iter().all(|(s, id, _)| *s == StatusCode::OK && *id == orders[0].1)
            && orders[0].2 == 1_350
            && shop.edge.orders.get(&orders[0].1).await.is_some(),
        format!("order {} at {} KES, the quoted price (A-I25)", orders[0].1, orders[0].2),
    );

    // Payment, sent three times: one payment, one signed receipt.
    let pay_body = json!({ "orderId": orders[0].1, "rail": "sandbox" });
    let mut receipts = Vec::new();
    for _ in 0..3 {
        let (status, _, paid) = shop
            .send(shop.signed(shop.call("pay_order", "pay-1", Some(&payment), &pay_body)))
            .await;
        receipts.push((status, paid["receipt"].clone()));
    }
    checks.check(
        "one payment per request: the retries are answered from the action ledger",
        desk.settled.load(Ordering::SeqCst) == 1
            && receipts.iter().all(|(s, r)| *s == StatusCode::OK && *r == receipts[0].1),
        format!("{} settlement at the rail for 3 requests", desk.settled.load(Ordering::SeqCst)),
    );
    let receipt = receipts[0].1.clone();
    let key = keys.edge.public_key();
    let mut tampered = receipt.clone();
    tampered["reference"] = json!("another-order");
    checks.check(
        "the payment receipt is signed by the edge's receipt key and fails once changed",
        verify_value(&receipt, &key).is_ok() && verify_value(&tampered, &key).is_err(),
        format!("receipt {} for {}", receipt["id"], receipt["reference"]),
    );
    let grant = json!({ "scope": "payment_data", "policyVersion": "1", "jurisdiction": "KE" });
    let (_, _, consent) = shop.send(shop.post("/awp/v2/consent", &grant)).await;
    checks.check(
        "consent receipts are issued and signed",
        verify_value(&consent, &key).is_ok() && consent["scope"] == "payment_data",
        format!("consent receipt {}", consent["id"]),
    );

    // The large order: two wedding cakes, 50,000 KES.
    let large_quote = json!({ "items": [{ "sku": "wedding-cake", "quantity": 2 }] });
    let (_, _, quote) =
        shop.send(shop.signed(shop.call("get_quote", "q-2", None, &large_quote))).await;
    let (_, _, order) = shop
        .send(shop.signed(shop.call(
            "place_order",
            "order-2",
            Some(&payment),
            &json!({ "quoteId": quote["id"] }),
        )))
        .await;
    let large = json!({ "orderId": order["id"], "rail": "sandbox" });
    let (status, headers, _) =
        shop.send(shop.signed(shop.call("pay_order", "pay-2", Some(&payment), &large))).await;
    let location = headers.get(header::LOCATION).and_then(|v| v.to_str().ok()).map(str::to_string);
    let mut completed = false;
    if status == StatusCode::ACCEPTED {
        shop.approve_pending().await?;
        if let Some(location) = &location {
            let (_, _, done) = shop.send(shop.signed(shop.get(location))).await;
            completed = done["state"] == "completed";
        }
    } else {
        checks.gap(
            "no capability the packs declare carries an amount threshold \
             (`ApprovalHint::AboveAmount`), and the owner has no way to set one; `place_order` and \
             `pay_order` take no amount the edge's policy could read, so a large order is never \
             held at the edge",
        );
    }
    checks.check(
        "the large one answers 202 and completes after the owner approves",
        status == StatusCode::ACCEPTED && completed,
        format!("{} KES: {status}", quote["total"]),
    );
    checks.gap(
        "no AP2 rail reaches the edge: nothing implements `PaymentDesk` over AP2 (main has \
         `PaymentLinks`, payment instructions only), `pay_order`'s schema `{orderId, rail}` \
         refuses any other field, so a buyer's payment mandate cannot travel with the call, and a \
         desk cannot mark an order paid (`OrderBook::set_status` is private)",
    );
    Ok(checks)
}
