//! Commerce on the agent edge (spec 3.5 §7.4, §7.5 "Commerce", §7.14, S9, A-I7, A-I25): the
//! owner's amount thresholds hold large orders, bookings and payments for the owner; an order
//! is paid once under the buyer's AP2 payment mandate, with a receipt signed by the edge's
//! `receipt` key; a mandate for another cart, without the buyer's authorisation or used
//! before pays nothing.

mod common;

use std::sync::Arc;

use adk_awp::signing::jws::{DocumentSigner, verify_value};
use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use adk_payments::protocol::ap2::{
    AuthorizationArtifact, CartMandate, IntentMandate, MerchantAuthorizationVerifier,
    PaymentMandate, PaymentMandateContents, PaymentResponse, UserAuthorizationVerifier,
    VerifiedAuthorization,
};
use adk_site::pipeline::{Change, EntryPoint, PipelineOutcome};
use adk_website::edge::ap2::{Ap2Desk, SandboxProcessor, SiteCartVerifier, in_memory_journal};
use adk_website::edge::handlers::{InMemoryCalendar, PaymentDesk, Slot};
use adk_website::edge::token::{PaymentRails, TokenDesk};
use adk_website::kinds::CharterInputs;
use adk_website::onboarding::build::SitePlan;
use adk_website::packs::ApprovalThresholds;
use adk_website::team::AgentRole;
use awp_types::{
    Channel, ContentId, ContentItem, ContentKind, ContentState, Markdown, Role, SiteKind,
};
use axum::http::{StatusCode, header};
use common::edge::{EdgeSite, Shop, edge_shop, site_keys};
use common::*;
use serde_json::{Value, json};

const DOMAIN: &str = "juniper.example";
const MERCHANT: &str = "Juniper Bakery";

/// Commits the kind's charter and publishes release 1: the plan's pages and `extra`.
async fn launch(site: &Site, extra: Vec<ContentItem>) {
    let mut inputs = CharterInputs::new(
        site.site.id,
        MERCHANT,
        OWNER,
        "Fresh bread every morning.",
        vec!["en".parse().unwrap()],
        "KES",
    );
    inputs.p1_channels = vec![Channel::Sms];
    let charter = site.template.draft_charter(&site.team, &inputs);
    site.commit(&charter, &[(Role::Owner, OWNER)]).await;
    let mut items = SitePlan::new(&charter, MERCHANT).items(&charter, &[], &["KE".to_string()]);
    items.extend(extra);
    let change = Change {
        items,
        media: Vec::new(),
        entry: EntryPoint::SiteAgent { agent: AgentRole::Steward.principal(&site.site) },
        proposer: AgentRole::Steward.principal(&site.site),
        job: "onboarding".into(),
        action_class: "publish".into(),
    };
    let outcome = site.runtime.pipeline().run(change).await.unwrap();
    assert!(matches!(outcome, PipelineOutcome::Published(_)), "release 1: {outcome:?}");
}

fn product(sku: &str, title: &str, price: u64) -> ContentItem {
    ContentItem {
        id: ContentId(format!("products/{sku}/en")),
        kind: ContentKind::Product,
        slug: sku.into(),
        language: "en".parse().unwrap(),
        fields: [
            ("title".to_string(), json!(title)),
            ("summary".to_string(), json!(title)),
            ("sku".to_string(), json!(sku)),
            ("price".to_string(), json!(price)),
            ("currency".to_string(), json!("KES")),
            ("goal".to_string(), json!("sell")),
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

async fn shop(shop: Shop) -> (Site, EdgeSite) {
    let site = Site::new(SiteKind::Ecommerce, MERCHANT, DOMAIN, &["en"]).await;
    launch(
        &site,
        vec![product("rye-loaf", "Rye loaf", 450), product("wedding-cake", "Wedding cake", 25_000)],
    )
    .await;
    let edge = edge_shop(&site, DOMAIN, shop).await;
    (site, edge)
}

/// A signed buyer's quote for `quantity` of `sku`, then an order against it with `key`.
async fn order(edge: &EdgeSite, payment: &str, sku: &str, quantity: u64, key: &str) -> Value {
    let body = json!({ "items": [{ "sku": sku, "quantity": quantity }] });
    let (_, _, quote) =
        edge.send(edge.signed(edge.call("get_quote", &format!("q-{key}"), None, &body))).await;
    let (status, _, order) = edge
        .send(edge.signed(edge.call(
            "place_order",
            key,
            Some(payment),
            &json!({ "quoteId": quote["id"] }),
        )))
        .await;
    assert_eq!(status, StatusCode::OK, "{order}");
    order
}

/// The buyer's credentials provider: authorises a payment mandate as
/// `buyer-authorized:{mandate id}`.
struct BuyerCredentials;

fn unauthorized(message: &str) -> AdkError {
    AdkError::new(ErrorComponent::Auth, ErrorCategory::Unauthorized, "test.buyer", message)
}

#[async_trait::async_trait]
impl UserAuthorizationVerifier for BuyerCredentials {
    async fn verify_intent_authorization(
        &self,
        _: &IntentMandate,
        _: &AuthorizationArtifact,
    ) -> Result<VerifiedAuthorization> {
        Err(unauthorized("no intents here"))
    }

    async fn verify_payment_authorization(
        &self,
        mandate: &PaymentMandate,
        artifact: &AuthorizationArtifact,
    ) -> Result<VerifiedAuthorization> {
        let expected =
            format!("buyer-authorized:{}", mandate.payment_mandate_contents.payment_mandate_id);
        if artifact.value == expected {
            Ok(VerifiedAuthorization::new("user_authorization", serde_json::Map::new()))
        } else {
            Err(unauthorized("not the buyer's authorisation"))
        }
    }
}

/// The buyer's payment mandate `id` for exactly `cart`, authorised by the buyer's
/// credentials provider when `authorized`.
fn mandate(cart: &CartMandate, id: &str, authorized: bool) -> Value {
    let details = &cart.contents.payment_request.details;
    let mandate = PaymentMandate {
        payment_mandate_contents: PaymentMandateContents {
            payment_mandate_id: id.to_string(),
            payment_details_id: details.id.clone(),
            payment_details_total: details.total.clone(),
            payment_response: PaymentResponse {
                request_id: details.id.clone(),
                method_name: "CARD".into(),
                details: None,
                shipping_address: None,
                shipping_option: None,
                payer_name: None,
                payer_email: None,
                payer_phone: None,
            },
            merchant_agent: cart.contents.merchant_name.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        },
        user_authorization: authorized.then(|| format!("buyer-authorized:{id}")),
    };
    serde_json::to_value(mandate).unwrap()
}

/// The site's AP2 desk over a sandbox processor, signing with the edge's `receipt` key.
async fn ap2_desk() -> (Arc<Ap2Desk>, Arc<SandboxProcessor>) {
    let identity = AdkIdentity::new(
        AppName::try_from("adk-website").unwrap(),
        UserId::try_from("juniper-example").unwrap(),
        SessionId::try_from("ap2").unwrap(),
    );
    let (transactions, evidence) = in_memory_journal(&identity).await.unwrap();
    let processor = Arc::new(SandboxProcessor::new(transactions.clone()));
    let desk = Ap2Desk::builder(MERCHANT, site_keys(DOMAIN).edge.clone())
        .processor(processor.clone())
        .buyer_authorization(Arc::new(BuyerCredentials))
        .journal(transactions, evidence, identity)
        .build()
        .unwrap();
    (Arc::new(desk), processor)
}

#[tokio::test]
async fn orders_above_the_owners_threshold_answer_202_and_complete_once_approved() {
    let (_site, edge) =
        shop(Shop { thresholds: ApprovalThresholds::commerce(20_000), ..Shop::default() }).await;
    let (_, _, manifest) = edge.send(edge.signed(edge.get("/awp/v2/manifest"))).await;
    let place = manifest["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "place_order")
        .cloned()
        .unwrap();
    assert_eq!(place["approval"], json!({ "mode": "above_amount", "amount": 20_000 }));

    let payment = edge.consent("payment_data").await;
    let small = order(&edge, &payment, "rye-loaf", 3, "o-small").await;
    assert_eq!(small["quote"]["total"], 1_350, "below the threshold it is placed at once");

    // Two wedding cakes, 50,000: the owner decides.
    let body = json!({ "items": [{ "sku": "wedding-cake", "quantity": 2 }] });
    let (_, _, quote) =
        edge.send(edge.signed(edge.call("get_quote", "q-large", None, &body))).await;
    let place = || {
        edge.signed(edge.call(
            "place_order",
            "o-large",
            Some(&payment),
            &json!({ "quoteId": quote["id"] }),
        ))
    };
    let (status, headers, held) = edge.send(place()).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{held}");
    assert!(edge.edge.orders.get("order-o-large").await.is_none(), "nothing placed while held");
    let (status, _, again) = edge.send(place()).await;
    assert_eq!(
        (status, &again["id"]),
        (StatusCode::ACCEPTED, &held["id"]),
        "a retry is the same held call"
    );
    let location = headers[header::LOCATION].to_str().unwrap().to_string();
    assert_eq!(edge.approve_pending().await, 1, "one card for the owner");
    let (_, _, done) = edge.send(edge.signed(edge.get(&location))).await;
    assert_eq!(done["state"], "completed", "{done}");
    assert_eq!(done["result"]["quote"]["total"], 50_000);
    assert_eq!(edge.edge.orders.get("order-o-large").await.unwrap().status, "awaiting_payment");
}

#[tokio::test]
async fn a_booking_above_the_owners_threshold_waits_for_the_owner() {
    let site = Site::new(SiteKind::Business, MERCHANT, DOMAIN, &["en"]).await;
    launch(&site, Vec::new()).await;
    let start = chrono::Utc::now() + chrono::Duration::days(2);
    let slot = |id: &str, hours: i64, price: Option<u64>| Slot {
        id: id.into(),
        starts_at: start + chrono::Duration::hours(hours),
        minutes: 60,
        price,
    };
    let calendar = InMemoryCalendar::new([
        slot("tasting", 0, Some(5_000)),
        slot("wedding-consult", 2, Some(30_000)),
        slot("walk-in", 4, None),
    ]);
    let edge = edge_shop(
        &site,
        DOMAIN,
        Shop {
            calendar: Some(Arc::new(calendar)),
            thresholds: ApprovalThresholds::commerce(20_000),
            ..Shop::default()
        },
    )
    .await;
    let contact = edge.consent("contact").await;
    let book = |key: &str, slot: &str| {
        let body = json!({ "slot": slot, "name": "Wanjiru", "email": "wanjiru@mail.example" });
        edge.signed(edge.call("book", key, Some(&contact), &body))
    };
    for (key, slot) in [("b-1", "tasting"), ("b-3", "walk-in")] {
        let (status, _, booked) = edge.send(book(key, slot)).await;
        assert_eq!((status, &booked["booked"]), (StatusCode::OK, &json!(true)), "{slot}: {booked}");
    }
    let (status, headers, held) = edge.send(book("b-2", "wedding-consult")).await;
    assert_eq!(status, StatusCode::ACCEPTED, "a 30,000 booking is the owner's: {held}");
    let location = headers[header::LOCATION].to_str().unwrap().to_string();
    assert_eq!(edge.approve_pending().await, 1);
    let (_, _, done) = edge.send(edge.signed(edge.get(&location))).await;
    assert_eq!(
        (&done["state"], &done["result"]["slot"]),
        (&json!("completed"), &json!("wedding-consult"))
    );
}

#[tokio::test]
async fn an_order_is_paid_once_under_the_buyers_ap2_mandate() {
    let (desk, processor) = ap2_desk().await;
    let (_site, edge) = shop(Shop {
        payments: Some(desk.clone() as Arc<dyn PaymentDesk>),
        thresholds: ApprovalThresholds::new().above("pay_order", 20_000),
        ..Shop::default()
    })
    .await;
    let payment = edge.consent("payment_data").await;
    let placed = order(&edge, &payment, "rye-loaf", 3, "o-1").await;
    let order_id = placed["id"].as_str().unwrap().to_string();

    // The order carries the site's signed cart, which only the site's key verifies.
    assert_eq!(placed["payment"]["rail"], "ap2");
    let cart: CartMandate =
        serde_json::from_value(placed["payment"]["cartMandate"].clone()).unwrap();
    assert_eq!(cart.contents.id, order_id);
    assert_eq!(cart.contents.merchant_name, MERCHANT);
    assert_eq!(
        cart.contents.payment_request.details.total.amount,
        adk_payments::protocol::ap2::PaymentCurrencyAmount::new("KES", 13.5),
        "the cart totals the quoted 1,350 (A-I25)"
    );
    let key = site_keys(DOMAIN).edge.clone();
    let carts = SiteCartVerifier::new(MERCHANT, key.kid(), key.public_key());
    let signature = AuthorizationArtifact::new(
        "merchant_authorization",
        cart.merchant_authorization.clone().unwrap(),
        "application/jose",
    );
    assert!(carts.verify_cart_authorization(&cart, &signature).await.is_ok());
    let mut altered = cart.clone();
    altered.contents.payment_request.details.total.amount =
        adk_payments::protocol::ap2::PaymentCurrencyAmount::new("KES", 1.0);
    assert!(carts.verify_cart_authorization(&altered, &signature).await.is_err());

    // Paid three times with one key: one payment, one receipt (A-I7).
    let pay = |key: &str, order: &str, mandate: Value| {
        let body = json!({ "orderId": order, "rail": "ap2", "mandate": mandate });
        edge.signed(edge.call("pay_order", key, Some(&payment), &body))
    };
    let mut receipts = Vec::new();
    let first = mandate(&cart, "pm-1", true);
    for _ in 0..3 {
        let (status, _, paid) = edge.send(pay("p-1", &order_id, first.clone())).await;
        assert_eq!(status, StatusCode::OK, "{paid}");
        receipts.push(paid);
    }
    assert!(receipts.iter().all(|r| r == &receipts[0]), "retries are answered from the record");
    assert_eq!(
        processor.executed(),
        std::slice::from_ref(&order_id),
        "one payment at the processor"
    );
    let receipt = &receipts[0]["receipt"];
    assert_eq!(
        (receipts[0]["status"].as_str(), receipt["amount"].as_u64()),
        (Some("paid"), Some(1_350))
    );
    assert_eq!(receipt["rail"], "ap2");
    assert!(verify_value(receipt, &key.public_key()).is_ok(), "signed by the receipt key");
    let mut tampered = receipt.clone();
    tampered["amount"] = json!(1);
    assert!(verify_value(&tampered, &key.public_key()).is_err());
    assert_eq!(edge.edge.orders.get(&order_id).await.unwrap().status, "paid");

    // A paid order is not paid again, under any key.
    let (_, _, again) = edge.send(pay("p-1-again", &order_id, mandate(&cart, "pm-9", true))).await;
    assert_eq!(again["error"]["code"], "payment_policy_rejected", "{again}");

    // A second order: no mandate, an unauthorised one, another order's cart and a used
    // mandate pay nothing.
    let second = order(&edge, &payment, "rye-loaf", 1, "o-2").await;
    let second_id = second["id"].as_str().unwrap().to_string();
    let second_cart: CartMandate =
        serde_json::from_value(second["payment"]["cartMandate"].clone()).unwrap();
    let body = json!({ "orderId": second_id, "rail": "ap2" });
    let (_, _, bare) =
        edge.send(edge.signed(edge.call("pay_order", "p-2a", Some(&payment), &body))).await;
    for (key, mandate, why, cause) in [
        (
            "p-2b",
            mandate(&second_cart, "pm-2", false),
            "without the buyer's authorisation",
            "user authorization is required",
        ),
        (
            "p-2c",
            {
                let mut forged = mandate(&second_cart, "pm-2", true);
                forged["user_authorization"] = json!("someone-else");
                forged
            },
            "authorised by someone else",
            "not the buyer's authorisation",
        ),
        ("p-2d", mandate(&cart, "pm-3", true), "for another order's cart", "payment_details_id"),
        ("p-2e", mandate(&second_cart, "pm-1", true), "a mandate already used", "already executed"),
    ] {
        let (_, _, refused) = edge.send(pay(key, &second_id, mandate)).await;
        assert_eq!(refused["error"]["code"], "payment_policy_rejected", "{why}: {refused}");
        let message = refused["error"]["message"].as_str().unwrap_or_default();
        assert!(message.contains(cause), "{why}: {message}");
    }
    assert_eq!(bare["error"]["code"], "payment_policy_rejected", "{bare}");
    assert_eq!(processor.executed().len(), 1, "nothing more was paid");
    let (status, _, paid) =
        edge.send(pay("p-2", &second_id, mandate(&second_cart, "pm-2", true))).await;
    assert_eq!((status, &paid["status"]), (StatusCode::OK, &json!("paid")), "{paid}");

    // A payment above the owner's threshold waits for the owner, then completes once.
    let large = order(&edge, &payment, "wedding-cake", 2, "o-3").await;
    let large_id = large["id"].as_str().unwrap().to_string();
    let large_cart: CartMandate =
        serde_json::from_value(large["payment"]["cartMandate"].clone()).unwrap();
    let (status, headers, held) =
        edge.send(pay("p-3", &large_id, mandate(&large_cart, "pm-4", true))).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{held}");
    assert_eq!(processor.executed().len(), 2, "nothing paid while held");
    let location = headers[header::LOCATION].to_str().unwrap().to_string();
    assert_eq!(edge.approve_pending().await, 1);
    let (_, _, done) = edge.send(edge.signed(edge.get(&location))).await;
    assert_eq!(
        (&done["state"], &done["result"]["status"]),
        (&json!("completed"), &json!("paid")),
        "{done}"
    );
    assert_eq!(processor.executed(), [order_id, second_id, large_id.clone()]);
    assert_eq!(edge.edge.orders.get(&large_id).await.unwrap().status, "paid");
}

#[tokio::test]
async fn the_desk_needs_a_processor_a_buyer_verifier_and_a_journal() {
    let key: Arc<dyn DocumentSigner> = site_keys(DOMAIN).edge.clone();
    let error = Ap2Desk::builder(MERCHANT, key).build().unwrap_err();
    assert!(error.message.contains("payment processor"), "{error}");
}

#[tokio::test]
async fn another_buyer_learns_nothing_of_an_order() {
    let (_site, edge) = shop(Shop::default()).await;
    let payment = edge.consent("payment_data").await;
    let placed = order(&edge, &payment, "rye-loaf", 3, "o-1").await;
    let id = json!({ "orderId": placed["id"] });
    let (status, _, own) =
        edge.send(edge.signed(edge.call("order_status", "s-1", None, &id))).await;
    assert_eq!((status, &own["total"]), (StatusCode::OK, &json!(1_350)), "{own}");
    let (status, _, theirs) =
        edge.send(edge.signed_by_other(edge.call("order_status", "s-2", None, &id))).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "another buyer reads no order: {theirs}");

    // A return asked by another buyer is held, as every return is, and once the owner
    // approves it finds no order of theirs.
    let ret = json!({ "orderId": placed["id"], "reason": "not mine" });
    let (status, headers, _) =
        edge.send(edge.signed_by_other(edge.call("request_return", "r-1", None, &ret))).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let location = headers[header::LOCATION].to_str().unwrap().to_string();
    assert_eq!(edge.approve_pending().await, 1);
    let (_, _, done) = edge.send(edge.signed_by_other(edge.get(&location))).await;
    assert_eq!(done["error"]["code"], "resource_not_found", "{done}");
    let order = edge.edge.orders.get(placed["id"].as_str().unwrap()).await.unwrap();
    assert_eq!(order.status, "awaiting_payment", "the buyer's order is unchanged");
}

/// A desk that takes a while to settle, and counts what it takes.
struct SlowDesk(std::sync::atomic::AtomicUsize);

#[async_trait::async_trait]
impl PaymentDesk for SlowDesk {
    async fn take(
        &self,
        call: &adk_awp::v2::capability::CapabilityCall,
    ) -> std::result::Result<Value, adk_awp::v2::capability::CapabilityError> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        Ok(json!({ "status": "paid", "orderId": call.input["orderId"] }))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_payments_racing_on_one_order_pay_it_once() {
    let desk = Arc::new(SlowDesk(std::sync::atomic::AtomicUsize::new(0)));
    let (_site, edge) =
        shop(Shop { payments: Some(desk.clone() as Arc<dyn PaymentDesk>), ..Shop::default() })
            .await;
    let edge = Arc::new(edge);
    let payment = edge.consent("payment_data").await;
    let placed = order(&edge, &payment, "rye-loaf", 3, "o-1").await;
    let body = json!({ "orderId": placed["id"], "rail": "card", "credential": "tok_1" });
    let pay = |key: &'static str| {
        let (edge, payment, body) = (edge.clone(), payment.clone(), body.clone());
        tokio::spawn(async move {
            edge.send(edge.signed(edge.call("pay_order", key, Some(&payment), &body))).await
        })
    };
    let (first, second) = (pay("p-1"), pay("p-2"));
    let answers = [first.await.unwrap(), second.await.unwrap()];
    let paid = answers
        .iter()
        .filter(|(status, _, body)| *status == StatusCode::OK && body["status"] == "paid")
        .count();
    assert_eq!(paid, 1, "one payment proceeds: {answers:?}");
    assert!(
        answers.iter().any(|(_, _, body)| body["error"]["code"] == "payment_policy_rejected"),
        "the other is refused: {answers:?}"
    );
    assert_eq!(desk.0.load(std::sync::atomic::Ordering::SeqCst), 1, "the desk took one payment");
    let order = edge.edge.orders.get(placed["id"].as_str().unwrap()).await.unwrap();
    assert_eq!(order.status, "paid");
}

/// The owner's processor for a token rail: redeems tokens through the sandbox, declining any
/// that starts with `tok_declined`, and remembers what it was handed.
struct TokenProcessor {
    sandbox: SandboxProcessor,
    transactions: Arc<dyn adk_payments::kernel::TransactionStore>,
    seen: std::sync::Mutex<Vec<(String, String, i64, Value)>>,
}

#[async_trait::async_trait]
impl adk_payments::kernel::PaymentExecutionService for TokenProcessor {
    async fn execute_payment(
        &self,
        command: adk_payments::kernel::ExecutePaymentCommand,
    ) -> Result<adk_payments::kernel::PaymentExecutionResult> {
        let token = command
            .selected_payment_method
            .as_ref()
            .and_then(|method| method.reference.clone())
            .unwrap_or_default();
        let envelope = serde_json::to_value(&command.extensions).unwrap();
        self.seen.lock().unwrap().push((
            command.context.transaction_id.as_str().to_string(),
            token.clone(),
            command.amount.amount_minor,
            envelope,
        ));
        if token.starts_with("tok_declined") {
            let lookup = adk_payments::kernel::TransactionLookup {
                transaction_id: command.context.transaction_id.clone(),
                session_identity: command.context.session_identity.clone(),
            };
            let transaction = self.transactions.get(lookup).await?.expect("an open transaction");
            return Ok(adk_payments::kernel::PaymentExecutionResult {
                outcome: adk_payments::kernel::PaymentExecutionOutcome::Failed,
                transaction,
                order: None,
                intervention: None,
                generated_evidence_refs: Vec::new(),
            });
        }
        self.sandbox.execute_payment(command).await
    }

    async fn sync_payment_outcome(
        &self,
        command: adk_payments::kernel::SyncPaymentOutcomeCommand,
    ) -> Result<adk_payments::domain::TransactionRecord> {
        self.sandbox.sync_payment_outcome(command).await
    }
}

/// An order is paid once with the single-use token the buyer's payment provider issued, on a
/// token rail beside the AP2 desk; the token reaches the owner's processor and nothing else,
/// and a call on another rail, without a token, or with a declined token pays nothing
/// (spec 3.5 §7.5 "Commerce", S9).
#[tokio::test]
async fn an_order_is_paid_once_with_the_buyers_token_beside_ap2() {
    let identity = AdkIdentity::new(
        AppName::try_from("adk-website").unwrap(),
        UserId::try_from("juniper-example").unwrap(),
        SessionId::try_from("tokens").unwrap(),
    );
    let (transactions, _) = in_memory_journal(&identity).await.unwrap();
    let processor = Arc::new(TokenProcessor {
        sandbox: SandboxProcessor::new(transactions.clone()),
        transactions: transactions.clone(),
        seen: std::sync::Mutex::new(Vec::new()),
    });
    let key = site_keys(DOMAIN).edge.clone();
    let tokens = TokenDesk::builder(MERCHANT, key.clone())
        .rail("card")
        .processor(processor.clone())
        .journal(transactions.clone(), identity.clone())
        .build()
        .unwrap();
    let (ap2, _) = ap2_desk().await;
    let rails = PaymentRails::new()
        .rail("ap2", ap2 as Arc<dyn PaymentDesk>)
        .rail("card", Arc::new(tokens) as Arc<dyn PaymentDesk>);
    let (_site, edge) =
        shop(Shop { payments: Some(Arc::new(rails) as Arc<dyn PaymentDesk>), ..Shop::default() })
            .await;
    let payment = edge.consent("payment_data").await;
    let placed = order(&edge, &payment, "rye-loaf", 3, "o-1").await;
    let order_id = placed["id"].as_str().unwrap().to_string();
    assert_eq!(placed["payment"]["rail"], "ap2", "the AP2 desk still signs the cart: {placed}");

    let pay = |key: &str, order: &str, rail: &str, token: Option<&str>| {
        let mut body = json!({ "orderId": order, "rail": rail });
        if let Some(token) = token {
            body["credential"] = json!(token);
        }
        edge.signed(edge.call("pay_order", key, Some(&payment), &body))
    };
    let (_, _, bare) = edge.send(pay("p-0", &order_id, "card", None)).await;
    assert_eq!(bare["error"]["code"], "payment_policy_rejected", "{bare}");
    assert!(bare["error"]["message"].as_str().unwrap().contains("credential"), "{bare}");
    let (_, _, other) = edge.send(pay("p-00", &order_id, "mpesa", Some("tok_1"))).await;
    assert!(other["error"]["message"].as_str().unwrap().contains("ap2, card"), "{other}");
    assert!(processor.seen.lock().unwrap().is_empty(), "nothing reached the processor");

    // Paid three times with one key: one payment, one receipt (A-I7).
    let mut answers = Vec::new();
    for _ in 0..3 {
        let (status, _, paid) = edge.send(pay("p-1", &order_id, "card", Some("tok_1"))).await;
        assert_eq!(status, StatusCode::OK, "{paid}");
        answers.push(paid);
    }
    assert!(answers.iter().all(|a| a == &answers[0]), "retries are answered from the record");
    let paid = &answers[0];
    assert_eq!((paid["status"].as_str(), paid["rail"].as_str()), (Some("paid"), Some("card")));
    let receipt = &paid["receipt"];
    assert_eq!((receipt["amount"].as_u64(), receipt["rail"].as_str()), (Some(1_350), Some("card")));
    assert!(verify_value(receipt, &key.public_key()).is_ok(), "signed by the receipt key");
    assert!(!paid.to_string().contains("tok_1"), "the answer carries no token: {paid}");
    {
        let seen = processor.seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "one payment at the processor");
        let (transaction, token, amount, envelope) = &seen[0];
        assert_eq!(
            (transaction.as_str(), token.as_str(), *amount),
            (order_id.as_str(), "tok_1", 1_350)
        );
        assert!(envelope.to_string().contains("\"p-1\""), "the idempotency key: {envelope}");
    }
    let record = transactions
        .get(adk_payments::kernel::TransactionLookup {
            transaction_id: adk_payments::domain::TransactionId::from(order_id.as_str()),
            session_identity: Some(identity.clone()),
        })
        .await
        .unwrap()
        .expect("the journal keeps the order's transaction");
    assert!(!serde_json::to_string(&record).unwrap().contains("tok_1"), "no token in the journal");
    assert_eq!(edge.edge.orders.get(&order_id).await.unwrap().status, "paid");
    let (_, _, again) = edge.send(pay("p-1-again", &order_id, "card", Some("tok_2"))).await;
    assert_eq!(again["error"]["code"], "payment_policy_rejected", "paid once: {again}");

    // A declined token pays nothing and leaves the order to be paid.
    let second = order(&edge, &payment, "rye-loaf", 1, "o-2").await;
    let second_id = second["id"].as_str().unwrap().to_string();
    let (_, _, declined) = edge.send(pay("p-2a", &second_id, "card", Some("tok_declined_1"))).await;
    assert!(declined["error"]["message"].as_str().unwrap().contains("declined"), "{declined}");
    assert_eq!(edge.edge.orders.get(&second_id).await.unwrap().status, "awaiting_payment");
    let (status, _, paid) = edge.send(pay("p-2b", &second_id, "card", Some("tok_3"))).await;
    assert_eq!((status, &paid["status"]), (StatusCode::OK, &json!("paid")), "{paid}");
}

#[tokio::test]
async fn the_token_desk_needs_a_rail_a_processor_and_a_journal() {
    let key: Arc<dyn DocumentSigner> = site_keys(DOMAIN).edge.clone();
    let error = TokenDesk::builder(MERCHANT, key.clone()).build().unwrap_err();
    assert!(error.message.contains("rail"), "{error}");
    let error = TokenDesk::builder(MERCHANT, key).rail("card").build().unwrap_err();
    assert!(error.message.contains("payment processor"), "{error}");
}
