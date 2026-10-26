//! S9 — agent-to-agent business (spec 3.5 §5 S9, §7.4, §7.5, §7.14, A-I7, A-I8, A-I25).
//!
//! A buyer agent, signed with Web Bot Auth, follows the shop's contract to its manifest,
//! takes a quote, books an order (non-idempotent) and pays for it under an AP2 sandbox
//! mandate; a second, large booking exceeds the owner's threshold. Every request is sent
//! three times: once, then retried twice.
//!
//! Both sides are `adk-payments`' AP2: the shop is the merchant through the product's AP2
//! desk (its signed cart with each order, the buyer's mandate checked against it, a sandbox
//! processor that moves no money), and the buyer pays from its own wallet through the AP2
//! rail, its credentials provider authorising each mandate.
//!
//! Pass condition: one booking and one payment per request; the large one answers 202,
//! completes after the owner approves on the phone; consent and signed receipts issued;
//! retries answered from the action ledger.

use std::sync::{Arc, Mutex};

use adk_awp::signing::jws::{DocumentSigner, verify_value};
use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result as AdkResult, SubjectRef};
use adk_payments::kernel::PaymentExecutionOutcome;
use adk_payments::protocol::ap2::{
    AuthorizationArtifact, CartMandate, IntentMandate, PaymentMandate, PaymentMandateContents,
    UserAuthorizationVerifier, VerifiedAuthorization,
};
use adk_payments::wallet::{
    Ap2MerchantEndpoint, Ap2Rail, Ap2RailData, Ap2Settlement, Ap2UserAuthorizer,
    InMemoryWalletProvider, Mandate, Payee, PayeeRule, PaymentRequest, RailRef, SpendCategory,
    Wallet, WalletCaller, WalletPolicy,
};
use adk_website::edge::ap2::{Ap2Desk, SandboxProcessor, in_memory_journal};
use adk_website::packs::ApprovalThresholds;
use anyhow::{Result, anyhow};
use awp_types::{ContentId, ContentItem, ContentKind, ContentState, Markdown, SiteKind};
use axum::http::{StatusCode, header};
use serde_json::{Value, json};

use crate::checks::Checks;
use crate::fixture::edge::{EdgeOptions, EdgeSite, edge_with, site_keys};
use crate::fixture::*;
use crate::live::Mode;

const DOMAIN: &str = "juniper.example";
const MERCHANT: &str = "Juniper Bakery";
/// Bookings above 20,000 KES cents (200 KES) wait for the owner.
const THRESHOLD: u64 = 20_000;

/// The buyer's credentials provider: authorises each payment mandate the buyer's wallet
/// allowed, as `buyer-authorized:{mandate id}`; the shop's desk accepts nothing else.
struct BuyerCredentials;

fn unauthorized(message: &str) -> AdkError {
    AdkError::new(ErrorComponent::Auth, ErrorCategory::Unauthorized, "s9.buyer", message)
}

#[async_trait::async_trait]
impl Ap2UserAuthorizer for BuyerCredentials {
    async fn authorize(&self, contents: &PaymentMandateContents) -> AdkResult<String> {
        Ok(format!("buyer-authorized:{}", contents.payment_mandate_id))
    }
}

#[async_trait::async_trait]
impl UserAuthorizationVerifier for BuyerCredentials {
    async fn verify_intent_authorization(
        &self,
        _: &IntentMandate,
        _: &AuthorizationArtifact,
    ) -> AdkResult<VerifiedAuthorization> {
        Err(unauthorized("the buyer sends no intents"))
    }

    async fn verify_payment_authorization(
        &self,
        mandate: &PaymentMandate,
        artifact: &AuthorizationArtifact,
    ) -> AdkResult<VerifiedAuthorization> {
        let expected =
            format!("buyer-authorized:{}", mandate.payment_mandate_contents.payment_mandate_id);
        if artifact.value == expected {
            Ok(VerifiedAuthorization::new("user_authorization", serde_json::Map::new()))
        } else {
            Err(unauthorized("not the buyer's credentials provider"))
        }
    }
}

/// The shop's AP2 surface as the buyer's wallet reaches it: `pay_order` on the edge, signed
/// by the buyer agent with its consent receipt, keyed by the mandate, sent three times.
struct ShopEndpoint {
    shop: Arc<EdgeSite>,
    consent: String,
    answers: Mutex<Vec<Vec<(StatusCode, Value)>>>,
}

#[async_trait::async_trait]
impl Ap2MerchantEndpoint for ShopEndpoint {
    async fn submit_payment_mandate(
        &self,
        cart: &CartMandate,
        mandate: PaymentMandate,
    ) -> AdkResult<Ap2Settlement> {
        let key = format!("pay-{}", mandate.payment_mandate_contents.payment_mandate_id);
        let body = json!({ "orderId": cart.contents.id, "rail": "ap2", "mandate": mandate });
        let mut answers = Vec::new();
        for _ in 0..3 {
            let request = self.shop.call("pay_order", &key, Some(&self.consent), &body);
            let (status, _, answer) = self.shop.send(self.shop.signed(request)).await;
            answers.push((status, answer));
        }
        self.answers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(answers.clone());
        let (status, answer) = answers.swap_remove(0);
        if status != StatusCode::OK || answer["status"] != "paid" {
            return Err(AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Forbidden,
                "s9.not_paid",
                format!("the shop answered {status}: {answer}"),
            ));
        }
        Ok(Ap2Settlement {
            outcome: PaymentExecutionOutcome::Completed,
            reference: cart.contents.id.clone(),
            receipt: None,
        })
    }
}

impl ShopEndpoint {
    /// The answers to the last payment, three of them.
    fn last(&self) -> Vec<(StatusCode, Value)> {
        self.answers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .last()
            .cloned()
            .unwrap_or_default()
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

/// The buyer's wallet: purchases from the bakery through the AP2 rail, its credentials
/// provider authorising each mandate.
fn buyer_wallet(endpoint: Arc<ShopEndpoint>) -> Result<Wallet> {
    let policy = WalletPolicy::new("KES").with_mandate(
        SpendCategory::Other,
        Mandate::new(1_000_000, 100_000, RailRef::ap2()).with_payee(PayeeRule::Id(MERCHANT.into())),
    );
    let provider = InMemoryWalletProvider::stored_value("buyer-treasury", "KES", 1_000_000);
    Ok(Wallet::builder("buyer-agency", policy, Arc::new(provider))
        .rail(Arc::new(Ap2Rail::new(endpoint, Arc::new(BuyerCredentials))))
        .owner(SubjectRef::human("buyer-owner@example.com"))
        .build()?)
}

/// The buyer's payment for `order` under its cart.
fn purchase(order: &Value, key: &str) -> Result<PaymentRequest> {
    let cart: CartMandate = serde_json::from_value(order["payment"]["cartMandate"].clone())
        .map_err(|e| anyhow!("the order carries no AP2 cart: {e}"))?;
    let total = order["quote"]["total"].as_u64().ok_or_else(|| anyhow!("an order total"))?;
    Ok(PaymentRequest::new(key, SpendCategory::Other, Payee::new(MERCHANT), total, "KES")
        .with_description("bakery order")
        .with_rail_data(Ap2RailData { cart_mandate: cart }.into_value()))
}

/// Runs S9.
pub async fn run(_mode: Mode) -> Result<Checks> {
    let mut checks = Checks::default();
    let mut site = Site::new(SiteKind::Ecommerce, MERCHANT, DOMAIN, &["en"]).await?;
    site.launch_signed(
        MERCHANT,
        "KES",
        vec![product("rye-loaf", "Rye loaf", 450), product("wedding-cake", "Wedding cake", 25_000)],
    )
    .await?;
    let keys = site_keys(DOMAIN)?;

    // The owner's AP2 desk: carts and receipts signed with the edge's receipt key, the
    // buyers' mandates checked by their credentials provider's authorisation, payments
    // executed by a sandbox processor.
    let identity = AdkIdentity::new(
        AppName::try_from("adk-website")?,
        UserId::try_from("juniper-example")?,
        SessionId::try_from("ap2")?,
    );
    let (transactions, evidence) = in_memory_journal(&identity).await?;
    let processor = Arc::new(SandboxProcessor::new(transactions.clone()));
    let desk = Ap2Desk::builder(MERCHANT, keys.edge.clone())
        .processor(processor.clone())
        .buyer_authorization(Arc::new(BuyerCredentials))
        .journal(transactions, evidence, identity)
        .build()?;
    let shop = Arc::new(
        edge_with(
            &site,
            DOMAIN,
            EdgeOptions {
                payments: Some(Arc::new(desk)),
                thresholds: ApprovalThresholds::commerce(THRESHOLD),
                ..EdgeOptions::default()
            },
        )
        .await?,
    );

    // The buyer follows the contract to the manifest.
    let (_, _, contract) = shop.send(shop.get("/.well-known/awp.json")).await;
    let manifest_url = contract["surfaces"]["manifest"].as_str().unwrap_or_default().to_string();
    let manifest_path =
        url::Url::parse(&manifest_url).map(|u| u.path().to_string()).unwrap_or_default();
    let (_, _, manifest) = shop.send(shop.signed(shop.get(&manifest_path))).await;
    let booking = manifest["capabilities"]
        .as_array()
        .and_then(|list| list.iter().find(|c| c["name"] == "place_order"))
        .map(|c| c["approval"].clone())
        .unwrap_or_default();
    checks.check(
        "the buyer follows the signed contract to the manifest, as a known agent",
        manifest_path == "/awp/v2/manifest"
            && manifest["tier"] == "known"
            && booking == json!({ "mode": "above_amount", "amount": THRESHOLD }),
        format!("{manifest_url}; place_order is held above {booking}"),
    );

    // A quote, then a booking against it, sent three times: one booking.
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
        orders.push((status, order));
    }
    let first = orders[0].1.clone();
    checks.check(
        "one booking per request: the retries are answered from the record",
        orders.iter().all(|(s, order)| *s == StatusCode::OK && *order == first)
            && first["quote"]["total"] == 1_350
            && first["payment"]["rail"] == "ap2"
            && shop.edge.orders.get(first["id"].as_str().unwrap_or_default()).await.is_some(),
        format!(
            "order {} at {} KES cents, the quoted price (A-I25), with the shop's signed AP2 cart",
            first["id"], first["quote"]["total"]
        ),
    );

    // The buyer pays from its wallet under an AP2 mandate; the endpoint sends each payment
    // three times: one payment, one signed receipt.
    let endpoint = Arc::new(ShopEndpoint {
        shop: shop.clone(),
        consent: payment.clone(),
        answers: Mutex::new(Vec::new()),
    });
    let wallet = buyer_wallet(endpoint.clone())?;
    let buyer = WalletCaller::agent("urn:adk:agent:buyer-agency/agent-desk");
    let paid = wallet.pay(purchase(&first, "buy-1")?, &buyer).await?;
    let answers = endpoint.last();
    checks.check(
        "one payment per request: the retries are answered from the action ledger",
        paid.is_settled()
            && processor.executed().len() == 1
            && answers.len() == 3
            && answers.iter().all(|(s, a)| *s == StatusCode::OK && *a == answers[0].1)
            && answers[0].1["status"] == "paid",
        format!(
            "{} payment at the processor for 3 requests; the buyer's wallet settled {}",
            processor.executed().len(),
            paid.record().amount
        ),
    );
    let receipt = answers.first().map(|(_, a)| a["receipt"].clone()).unwrap_or_default();
    let key = keys.edge.public_key();
    let mut tampered = receipt.clone();
    tampered["reference"] = json!("another-order");
    checks.check(
        "the payment receipt is signed by the edge's receipt key and fails once changed",
        verify_value(&receipt, &key).is_ok()
            && verify_value(&tampered, &key).is_err()
            && receipt["rail"] == "ap2"
            && receipt["amount"] == 1_350,
        format!("receipt {} for {}", receipt["id"], receipt["reference"]),
    );
    let grant = json!({ "scope": "payment_data", "policyVersion": "1", "jurisdiction": "KE" });
    let (_, _, consent) = shop.send(shop.post("/awp/v2/consent", &grant)).await;
    checks.check(
        "consent receipts are issued and signed",
        verify_value(&consent, &key).is_ok() && consent["scope"] == "payment_data",
        format!("consent receipt {}", consent["id"]),
    );

    // The large booking: two wedding cakes, 50,000 KES cents, over the owner's threshold.
    let large_quote = json!({ "items": [{ "sku": "wedding-cake", "quantity": 2 }] });
    let (_, _, quote) =
        shop.send(shop.signed(shop.call("get_quote", "q-2", None, &large_quote))).await;
    let large = json!({ "quoteId": quote["id"] });
    let mut held = Vec::new();
    for _ in 0..3 {
        let request = shop.call("place_order", "order-2", Some(&payment), &large);
        let (status, headers, body) = shop.send(shop.signed(request)).await;
        let location =
            headers.get(header::LOCATION).and_then(|v| v.to_str().ok()).map(str::to_string);
        held.push((status, location, body["id"].clone()));
    }
    let waiting =
        processor.executed().len() == 1 && shop.edge.orders.get("order-order-2").await.is_none();
    let approved = shop.approve_pending().await?;
    let mut done = Value::Null;
    if let Some(location) = held.first().and_then(|(_, location, _)| location.clone()) {
        let (_, _, body) = shop.send(shop.signed(shop.get(&location))).await;
        done = body;
    }
    let large_paid = match done["state"] == "completed" {
        true => wallet.pay(purchase(&done["result"], "buy-2")?, &buyer).await?.is_settled(),
        false => false,
    };
    checks.check(
        "the large one answers 202 and completes after the owner approves",
        held.iter().all(|(s, location, id)| {
            *s == StatusCode::ACCEPTED && location.is_some() && *id == held[0].2
        }) && waiting
            && approved == 1
            && done["state"] == "completed"
            && large_paid
            && processor.executed().len() == 2,
        format!(
            "{} KES cents: {:?} three times, nothing booked or paid while held; the owner \
             approved {approved}; then {} and paid",
            quote["total"],
            held.first().map(|(s, _, _)| *s),
            done["state"]
        ),
    );
    Ok(checks)
}
