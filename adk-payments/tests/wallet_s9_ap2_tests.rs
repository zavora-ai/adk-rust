//! Scenario S9's payment half (spec 3.5 §5 S9, §7.5, §7.14): purchases under AP2
//! mandates through the wallet's AP2 rail, against the AP2 adapter as the merchant.
//!
//! Every request is retried twice and the merchant executes one payment per request; a
//! booking over the mandate's per-payment limit is held (the edge answers it 202) and
//! completes once the owner approves; the merchant's receipt, issued through the receipt
//! hook, verifies and fails once tampered with.

#![cfg(all(feature = "wallet", feature = "ap2"))]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use adk_artifact::InMemoryArtifactService;
use adk_core::identity::{AdkIdentity, AppName, SessionId, UserId};
use adk_core::{AdkError, ApprovalDecision, ErrorCategory, ErrorComponent, Result, SubjectRef};
use adk_payments::domain::{
    CommerceActor, CommerceActorRole, CommerceMode, MerchantRef, ProtocolDescriptor,
    ProtocolExtensions, TransactionId, TransactionRecord, TransactionState,
};
use adk_payments::journal::{ArtifactBackedEvidenceStore, SessionBackedTransactionStore};
use adk_payments::kernel::{
    CancelCheckoutCommand, CommerceContext, CompleteCheckoutCommand, CreateCheckoutCommand,
    EvidenceStore, ExecutePaymentCommand, MerchantCheckoutService, OrderUpdateCommand,
    PaymentExecutionOutcome, PaymentExecutionResult, PaymentExecutionService,
    SyncPaymentOutcomeCommand, TransactionLookup, TransactionStore, UpdateCheckoutCommand,
};
use adk_payments::protocol::ap2::{
    Ap2Adapter, AuthorizationArtifact, CartMandate, IntentMandate, PaymentMandate,
    PaymentMandateContents, UserAuthorizationVerifier, VerifiedAuthorization,
};
use adk_payments::wallet::conformance::payment_rail_conformance;
use adk_payments::wallet::{
    Ap2MerchantEndpoint, Ap2Rail, Ap2RailData, Ap2Settlement, Ap2UserAuthorizer, HoldReason,
    InMemoryWalletProvider, Mandate, Payee, PayeeRule, PaymentOutcome, PaymentRequest,
    PaymentState, RailPayment, RailRef, ReceiptDetails, ReceiptSigner, ReceiptVerifier,
    SpendCategory, Wallet, WalletCaller, WalletPolicy, issue_receipt, verify_receipt,
};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use awp_types::Proof;
use chrono::Utc;
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;

const APP: &str = "merchant-app";
const USER: &str = "buyer-site";
const SESSION: &str = "session-1";
const MERCHANT: &str = "Studio Bookings";
const OWNER: &str = "owner@example.com";

fn unused<T>() -> Result<T> {
    Err(AdkError::tool("not used by these tests"))
}

/// The merchant's checkout and payment backend: records every payment it executes.
struct Kernel {
    transactions: Arc<dyn TransactionStore>,
    executed: Mutex<Vec<ExecutePaymentCommand>>,
}

impl Kernel {
    fn executions(&self) -> usize {
        self.executed.lock().unwrap().len()
    }
}

#[async_trait]
impl MerchantCheckoutService for Kernel {
    async fn create_checkout(&self, command: CreateCheckoutCommand) -> Result<TransactionRecord> {
        let context = command.context;
        let mut record = TransactionRecord::new(
            context.transaction_id,
            context.actor,
            context.merchant_of_record,
            context.mode,
            command.cart,
            Utc::now(),
        );
        record.session_identity = context.session_identity;
        self.transactions.upsert(record.clone()).await?;
        Ok(record)
    }
    async fn update_checkout(&self, command: UpdateCheckoutCommand) -> Result<TransactionRecord> {
        let lookup = TransactionLookup {
            transaction_id: command.context.transaction_id,
            session_identity: command.context.session_identity,
        };
        let mut record = self.transactions.get(lookup).await?.expect("the checkout exists");
        if let Some(cart) = command.cart {
            record.cart = cart;
        }
        record.recompute_safe_summary();
        self.transactions.upsert(record.clone()).await?;
        Ok(record)
    }
    async fn get_checkout(&self, lookup: TransactionLookup) -> Result<Option<TransactionRecord>> {
        self.transactions.get(lookup).await
    }
    async fn complete_checkout(&self, _: CompleteCheckoutCommand) -> Result<TransactionRecord> {
        unused()
    }
    async fn cancel_checkout(&self, _: CancelCheckoutCommand) -> Result<TransactionRecord> {
        unused()
    }
    async fn apply_order_update(&self, _: OrderUpdateCommand) -> Result<TransactionRecord> {
        unused()
    }
}

#[async_trait]
impl PaymentExecutionService for Kernel {
    async fn execute_payment(
        &self,
        command: ExecutePaymentCommand,
    ) -> Result<PaymentExecutionResult> {
        self.executed.lock().unwrap().push(command.clone());
        let lookup = TransactionLookup {
            transaction_id: command.context.transaction_id,
            session_identity: command.context.session_identity,
        };
        let mut record = self.transactions.get(lookup).await?.expect("the checkout exists");
        if record.state.can_transition_to(&TransactionState::Authorized) {
            record
                .transition_to(TransactionState::Authorized, Utc::now())
                .map_err(AdkError::from)?;
        }
        record.recompute_safe_summary();
        self.transactions.upsert(record.clone()).await?;
        Ok(PaymentExecutionResult {
            outcome: PaymentExecutionOutcome::Completed,
            transaction: record,
            order: None,
            intervention: None,
            generated_evidence_refs: Vec::new(),
        })
    }
    async fn sync_payment_outcome(
        &self,
        _: SyncPaymentOutcomeCommand,
    ) -> Result<TransactionRecord> {
        unused()
    }
}

/// The owner's credentials provider: authorises payment mandates the wallet allowed.
struct OwnerCredentials;

#[async_trait]
impl Ap2UserAuthorizer for OwnerCredentials {
    async fn authorize(&self, contents: &PaymentMandateContents) -> Result<String> {
        Ok(format!("owner-authorized:{}", contents.payment_mandate_id))
    }
}

/// The merchant accepts only payment mandates the owner's credentials provider authorised.
struct OwnerAuthorizationVerifier;

#[async_trait]
impl UserAuthorizationVerifier for OwnerAuthorizationVerifier {
    async fn verify_intent_authorization(
        &self,
        _: &IntentMandate,
        _: &AuthorizationArtifact,
    ) -> Result<VerifiedAuthorization> {
        unused()
    }
    async fn verify_payment_authorization(
        &self,
        mandate: &PaymentMandate,
        artifact: &AuthorizationArtifact,
    ) -> Result<VerifiedAuthorization> {
        let expected =
            format!("owner-authorized:{}", mandate.payment_mandate_contents.payment_mandate_id);
        if artifact.value == expected {
            Ok(VerifiedAuthorization::new("user_authorization", serde_json::Map::new()))
        } else {
            Err(AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::Unauthorized,
                "test.bad_user_auth",
                "not the owner's authorization",
            ))
        }
    }
}

fn identity() -> AdkIdentity {
    AdkIdentity::new(
        AppName::try_from(APP).unwrap(),
        UserId::try_from(USER).unwrap(),
        SessionId::try_from(SESSION).unwrap(),
    )
}

fn context(tx: &str, actor: &str, role: CommerceActorRole) -> CommerceContext {
    CommerceContext {
        transaction_id: TransactionId::from(tx),
        session_identity: Some(identity()),
        actor: CommerceActor {
            actor_id: actor.to_string(),
            role,
            display_name: None,
            tenant_id: None,
            extensions: ProtocolExtensions::default(),
        },
        merchant_of_record: MerchantRef {
            merchant_id: "studio".to_string(),
            legal_name: MERCHANT.to_string(),
            display_name: Some(MERCHANT.to_string()),
            statement_descriptor: None,
            country_code: None,
            website: None,
            extensions: ProtocolExtensions::default(),
        },
        payment_processor: None,
        mode: CommerceMode::HumanPresent,
        protocol: ProtocolDescriptor::ap2("v0.1-alpha"),
        extensions: ProtocolExtensions::default(),
    }
}

/// The merchant's signed cart for booking `tx` at `usd` dollars.
fn cart_mandate(tx: &str, usd: f64) -> CartMandate {
    serde_json::from_value(json!({
        "contents": {
            "id": tx,
            "user_cart_confirmation_required": false,
            "payment_request": {
                "method_data": [{ "supported_methods": "CARD" }],
                "details": {
                    "id": tx,
                    "display_items": [{ "label": "Studio session", "amount": { "currency": "USD", "value": usd } }],
                    "total": { "label": "Total", "amount": { "currency": "USD", "value": usd } }
                }
            },
            "cart_expiry": (Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
            "merchant_name": MERCHANT
        },
        "merchant_authorization": "signed-by-merchant",
        "timestamp": Utc::now().to_rfc3339()
    }))
    .unwrap()
}

/// The merchant agent's AP2 endpoint, in process: the AP2 adapter on the merchant's side.
struct LocalMerchant {
    adapter: Ap2Adapter,
}

#[async_trait]
impl Ap2MerchantEndpoint for LocalMerchant {
    async fn submit_payment_mandate(
        &self,
        cart: &CartMandate,
        mandate: PaymentMandate,
    ) -> Result<Ap2Settlement> {
        let context = context(&cart.contents.id, "buyer-agent", CommerceActorRole::AgentSurface);
        let result = self.adapter.submit_payment_mandate(context, mandate).await?;
        Ok(Ap2Settlement {
            outcome: result.outcome,
            reference: result.transaction.transaction_id.as_str().to_string(),
            receipt: None,
        })
    }
}

struct Market {
    kernel: Arc<Kernel>,
    merchant: Arc<LocalMerchant>,
}

impl Market {
    async fn new() -> Self {
        let sessions = Arc::new(InMemorySessionService::new());
        sessions
            .create(CreateRequest {
                app_name: APP.into(),
                user_id: USER.into(),
                session_id: Some(SESSION.into()),
                state: HashMap::new(),
            })
            .await
            .unwrap();
        let transactions: Arc<dyn TransactionStore> =
            Arc::new(SessionBackedTransactionStore::new(sessions));
        let evidence: Arc<dyn EvidenceStore> =
            Arc::new(ArtifactBackedEvidenceStore::new(Arc::new(InMemoryArtifactService::new())));
        let kernel = Arc::new(Kernel {
            transactions: transactions.clone(),
            executed: Mutex::new(Vec::new()),
        });
        let adapter = Ap2Adapter::new(kernel.clone(), kernel.clone(), transactions, evidence)
            .with_user_authorization_verifier(Arc::new(OwnerAuthorizationVerifier))
            .allow_unverified_authorizations();
        Self { kernel, merchant: Arc::new(LocalMerchant { adapter }) }
    }

    /// The merchant quotes `tx` and signs its cart.
    async fn quote(&self, tx: &str, usd: f64) -> CartMandate {
        let cart = cart_mandate(tx, usd);
        self.merchant
            .adapter
            .submit_cart_mandate(context(tx, "studio", CommerceActorRole::Merchant), cart.clone())
            .await
            .expect("the merchant accepts its own cart");
        cart
    }
}

fn booking(key: &str, cents: u64, cart: CartMandate) -> PaymentRequest {
    PaymentRequest::new(key, SpendCategory::Other, Payee::new(MERCHANT), cents, "USD")
        .with_description("studio booking")
        .with_rail_data(Ap2RailData { cart_mandate: cart }.into_value())
}

fn wallet(market: &Market, provider: InMemoryWalletProvider) -> Wallet {
    let policy = WalletPolicy::new("USD").with_mandate(
        SpendCategory::Other,
        Mandate::new(100_000, 15_000, RailRef::ap2()).with_payee(PayeeRule::Id(MERCHANT.into())),
    );
    Wallet::builder("buyer-site", policy, Arc::new(provider))
        .rail(Arc::new(Ap2Rail::new(market.merchant.clone(), Arc::new(OwnerCredentials))))
        .owner(SubjectRef::human(OWNER))
        .build()
        .unwrap()
}

#[tokio::test]
async fn s9_each_booking_pays_once_and_the_large_one_completes_after_the_owner_approves() {
    let market = Market::new().await;
    let provider = InMemoryWalletProvider::stored_value("treasury", "USD", 1_000_000);
    let wallet = wallet(&market, provider.clone());
    let buyer = WalletCaller::agent("urn:adk:agent:buyer-site/agent-desk");

    // A 120 USD booking, retried twice: one payment at the merchant.
    let cart = market.quote("booking-1", 120.0).await;
    let first = wallet.pay(booking("req-1", 12_000, cart.clone()), &buyer).await.unwrap();
    for _ in 0..2 {
        assert_eq!(
            wallet.pay(booking("req-1", 12_000, cart.clone()), &buyer).await.unwrap(),
            first
        );
    }
    let PaymentOutcome::Settled(paid) = first else { panic!("an in-mandate booking settles") };
    assert_eq!(market.kernel.executions(), 1);
    assert_eq!(paid.settlement.as_ref().unwrap().reference, "booking-1");
    assert_eq!(provider.available(), 1_000_000 - 12_000, "the treasury funded it once");

    // A 400 USD booking is over the 150 USD per-payment limit: held, retried twice, unpaid.
    let large = market.quote("booking-2", 400.0).await;
    let held = wallet.pay(booking("req-2", 40_000, large.clone()), &buyer).await.unwrap();
    let PaymentOutcome::Held(held) = held else { panic!("over the limit is held") };
    assert_eq!(
        held.hold,
        Some(HoldReason::OverPaymentLimit { max_per_payment: 15_000, requested: 40_000 })
    );
    for _ in 0..2 {
        assert!(
            wallet.pay(booking("req-2", 40_000, large.clone()), &buyer).await.unwrap().is_held()
        );
    }
    assert_eq!(market.kernel.executions(), 1, "nothing paid while held");

    // The owner approves on the phone; the booking completes, once.
    let done = wallet
        .decide(&held.id, &ApprovalDecision::approve(SubjectRef::human(OWNER)))
        .await
        .unwrap();
    assert_eq!(done.state, PaymentState::Settled);
    assert_eq!(market.kernel.executions(), 2);
    for _ in 0..2 {
        let again = wallet.pay(booking("req-2", 40_000, large.clone()), &buyer).await.unwrap();
        assert!(again.is_settled());
    }
    assert_eq!(
        market.kernel.executions(),
        2,
        "retries after approval are answered from the journal"
    );
    let executed: Vec<String> = market
        .kernel
        .executed
        .lock()
        .unwrap()
        .iter()
        .map(|command| command.context.transaction_id.as_str().to_string())
        .collect();
    assert_eq!(executed, vec!["booking-1", "booking-2"]);

    // The merchant's receipt for the booking, issued through the receipt hook, verifies.
    let key = TestReceiptKey(b"studio-receipt-key");
    let receipt = issue_receipt(
        &key,
        ReceiptDetails::new("book_session", "booking-2", 40_000, "USD").with_rail("ap2"),
        Utc::now(),
    )
    .await
    .unwrap();
    verify_receipt(&key, &receipt).await.expect("the receipt verifies");
    let mut tampered = receipt.clone();
    tampered.reference = "booking-3".into();
    assert!(verify_receipt(&key, &tampered).await.is_err(), "a changed receipt fails");
}

#[tokio::test]
async fn a_cart_from_another_merchant_or_at_another_total_is_refused_before_money_moves() {
    let market = Market::new().await;
    let provider = InMemoryWalletProvider::stored_value("treasury", "USD", 1_000_000);
    let wallet = wallet(&market, provider.clone());
    let buyer = WalletCaller::agent("urn:adk:agent:buyer-site/agent-desk");
    let cart = market.quote("booking-9", 120.0).await;
    // The request says 100 USD; the cart says 120 USD.
    let error = wallet.pay(booking("req-9", 10_000, cart.clone()), &buyer).await.unwrap_err();
    assert_eq!(error.code, adk_payments::wallet::WALLET_RAIL_DATA_MISMATCH_CODE);
    assert_eq!(market.kernel.executions(), 0);
    assert_eq!(provider.available(), 1_000_000);

    let rail = Ap2Rail::new(market.merchant.clone(), Arc::new(OwnerCredentials));
    let payment = |payee: &str, amount: u64| RailPayment {
        payment_id: format!("pm-{}", uuid::Uuid::new_v4().simple()),
        category: SpendCategory::Other,
        payee: Payee::new(payee),
        amount,
        currency: "USD".into(),
        description: "conformance".into(),
        rail_data: Some(Ap2RailData { cart_mandate: cart.clone() }.into_value()),
        debit: None,
    };
    payment_rail_conformance(&rail, payment(MERCHANT, 12_000), payment("Another Studio", 12_000))
        .await;
    assert_eq!(market.kernel.executions(), 1);
}

/// A stand-in for the merchant's delegated receipt key.
struct TestReceiptKey(&'static [u8]);

impl TestReceiptKey {
    fn mac(&self, document: &Value) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(self.0).unwrap();
        mac.update(document.to_string().as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }
}

#[async_trait]
impl ReceiptSigner for TestReceiptKey {
    async fn sign(&self, document: &Value) -> Result<Proof> {
        Ok(Proof::eddsa("studio-receipt", self.mac(document)))
    }
}

#[async_trait]
impl ReceiptVerifier for TestReceiptKey {
    async fn verify(&self, document: &Value, proof: &Proof) -> Result<()> {
        if proof.jws == self.mac(document) {
            Ok(())
        } else {
            Err(AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::Unauthorized,
                "receipt.invalid",
                "the receipt does not verify",
            ))
        }
    }
}
