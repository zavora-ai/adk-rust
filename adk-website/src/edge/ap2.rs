//! The site as an AP2 merchant (spec 3.5 §7.5 "Commerce", §7.14, S9): an order is paid under
//! the buyer's AP2 payment mandate, checked by `adk-payments`' merchant-side `Ap2Adapter`.
//!
//! | Step | Capability | What happens |
//! |---|---|---|
//! | Order | `place_order` | [`Ap2Desk::checkout`] signs the order's AP2 cart with the edge's `receipt` key, opens its transaction in the adapter, and returns the cart with the order as `payment.cartMandate` |
//! | Mandate | — | The buyer's wallet (`adk-payments`' `Ap2Rail`) builds the payment mandate for exactly that cart; the buyer's credentials provider authorises it |
//! | Payment | `pay_order` with `mandate` | The adapter checks the mandate against the cart, verifies the buyer's authorisation, refuses a mandate already used, and has the owner's payment processor execute it; the desk answers with a receipt signed by the `receipt` key, and the order is paid |
//!
//! | Part | Role |
//! |---|---|
//! | [`Ap2Desk`] | The [`PaymentDesk`] |
//! | [`SiteCartVerifier`] | Verifies the site's signature on its carts; the adapter's merchant check |
//! | [`SandboxProcessor`] | A payment processor for tests and sandboxes that moves no money |
//! | [`in_memory_journal`] | Transaction and evidence stores in memory, for tests and sandboxes |
//!
//! The owner's payment processor is an `adk-payments` `PaymentExecutionService`; the site
//! holds no custody of its own, and the desk picks no processor.

use std::sync::{Arc, PoisonError};

use adk_awp::signing::jws::{DocumentSigner, sign, verify};
use adk_awp::v2::capability::{CapabilityCall, CapabilityError};
use adk_core::identity::AdkIdentity;
use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use adk_payments::domain::{
    CommerceActor, CommerceActorRole, CommerceMode, MerchantRef, Money, ProtocolDescriptor,
    ProtocolExtensionEnvelope, ProtocolExtensions, TransactionId, TransactionRecord,
    TransactionState,
};
use adk_payments::guardrail::PaymentPolicySet;
use adk_payments::kernel::{
    CancelCheckoutCommand, CommerceContext, CompleteCheckoutCommand, CreateCheckoutCommand,
    EvidenceStore, ExecutePaymentCommand, MerchantCheckoutService, OrderUpdateCommand,
    PaymentExecutionOutcome, PaymentExecutionResult, PaymentExecutionService,
    SyncPaymentOutcomeCommand, TransactionLookup, TransactionStore, UpdateCheckoutCommand,
};
use adk_payments::protocol::ap2::{
    Ap2Adapter, AuthorizationArtifact, CRYPTOGRAPHICALLY_VERIFIED_CLAIM, CartContents, CartMandate,
    MerchantAuthorizationVerifier, PaymentCurrencyAmount, PaymentDetailsInit, PaymentItem,
    PaymentMandate, PaymentMandateLedger, PaymentMethodData, PaymentRequest as Ap2PaymentRequest,
    VERIFIED_MERCHANT_NAME_CLAIM, VerifiedAuthorization,
};
use adk_payments::wallet::{ReceiptDetails, issue_receipt};
use async_trait::async_trait;
use awp_types::{ErrorCode, Proof, PublicKeyJwk};
use chrono::Utc;
use serde_json::{Map, Value, json};

use super::adapters::ReceiptKey;
use super::handlers::{Order, PAID, PaymentDesk};

/// The rail name `pay_order` names for an AP2 payment.
pub const AP2_RAIL: &str = "ap2";

/// The extension field the adapter hands the payment processor as its deduplication key.
const IDEMPOTENCY_KEY_FIELD: &str = "idempotency_key";

fn protocol() -> ProtocolDescriptor {
    ProtocolDescriptor::ap2(adk_payments::AP2_ALPHA_BASELINE)
}

fn refused(message: impl Into<String>) -> CapabilityError {
    CapabilityError::refused(ErrorCode::PaymentPolicyRejected, message)
}

/// An adapter or store error as the buyer sees it: a refusal for anything the buyer or the
/// mandate caused, an internal error for an unavailable or failing backend.
fn answered(error: AdkError) -> CapabilityError {
    match error.category {
        ErrorCategory::Unavailable | ErrorCategory::Timeout | ErrorCategory::Internal => {
            CapabilityError::Failed(error.to_string())
        }
        _ => refused(error.message),
    }
}

fn unsupported(what: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::Unsupported,
        "website.ap2.unsupported",
        format!("the site's AP2 desk does not {what}; orders are paid through `pay_order`"),
    )
}

/// `minor` units of `currency` as an AP2 amount in major units.
fn amount(currency: &str, minor: u64) -> PaymentCurrencyAmount {
    let scale = Money::iso_minor_unit_scale(currency).unwrap_or(2);
    let divisor = 10f64.powi(i32::try_from(scale).unwrap_or(2));
    PaymentCurrencyAmount::new(currency, minor as f64 / divisor)
}

fn item(label: String, currency: &str, minor: u64) -> PaymentItem {
    PaymentItem { label, amount: amount(currency, minor), pending: None, refund_period: 30 }
}

/// Takes payment for the site's orders under AP2 mandates: the site is the AP2 merchant
/// (spec 3.5 §7.5 "Commerce", §7.14, S9).
///
/// `place_order` returns the order with the site's signed cart for it ([`checkout`]); the
/// buyer's `pay_order` carries the payment mandate for that cart as `mandate`. The adapter
/// checks the mandate's cart, total and expiry, verifies the buyer's user authorisation with
/// the configured verifier, consumes the mandate once, and has the owner's payment processor
/// execute it. The answer is `{status: "paid", orderId, receipt, ap2}`, the receipt signed by
/// the edge's `receipt` key; a mandate the adapter refuses answers
/// `payment_policy_rejected` and nothing is paid.
///
/// [`checkout`]: PaymentDesk::checkout
///
/// # Example
///
/// ```rust,ignore
/// let (transactions, evidence) = in_memory_journal(&identity).await?;
/// let desk = Ap2Desk::builder("Juniper Bakery", receipt_key)
///     .processor(owner_processor)
///     .buyer_authorization(credentials_provider_verifier)
///     .journal(transactions, evidence, identity)
///     .build()?;
/// let parts = SiteEdgeParts { payments: Some(Arc::new(desk)), ..parts };
/// ```
pub struct Ap2Desk {
    merchant: MerchantRef,
    key: Arc<dyn DocumentSigner>,
    adapter: Ap2Adapter,
    transactions: Arc<dyn TransactionStore>,
    journal: AdkIdentity,
    method: String,
}

impl std::fmt::Debug for Ap2Desk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ap2Desk")
            .field("merchant", &self.merchant.legal_name)
            .field("method", &self.method)
            .finish_non_exhaustive()
    }
}

/// Where the desk's AP2 transactions and mandates are kept.
struct Journal {
    transactions: Arc<dyn TransactionStore>,
    evidence: Arc<dyn EvidenceStore>,
    identity: AdkIdentity,
}

/// Builds an [`Ap2Desk`].
pub struct Ap2DeskBuilder {
    merchant: String,
    key: Arc<dyn DocumentSigner>,
    processor: Option<Arc<dyn PaymentExecutionService>>,
    buyers: Option<Arc<dyn adk_payments::protocol::ap2::UserAuthorizationVerifier>>,
    journal: Option<Journal>,
    policies: Option<PaymentPolicySet>,
    mandates: Option<Arc<dyn PaymentMandateLedger>>,
    method: String,
}

impl Ap2DeskBuilder {
    /// The owner's payment processor, which executes each payment mandate the adapter lets
    /// through. Required.
    #[must_use]
    pub fn processor(mut self, processor: Arc<dyn PaymentExecutionService>) -> Self {
        self.processor = Some(processor);
        self
    }

    /// Verifies the user authorisation on each buyer's payment mandate, such as the buyer's
    /// credentials provider's signature. Required: without it every mandate is refused.
    #[must_use]
    pub fn buyer_authorization(
        mut self,
        verifier: Arc<dyn adk_payments::protocol::ap2::UserAuthorizationVerifier>,
    ) -> Self {
        self.buyers = Some(verifier);
        self
    }

    /// Keeps the AP2 transactions in `transactions` and the mandates as evidence in
    /// `evidence`, under the session `identity`. Required; [`in_memory_journal`] gives
    /// in-memory ones.
    #[must_use]
    pub fn journal(
        mut self,
        transactions: Arc<dyn TransactionStore>,
        evidence: Arc<dyn EvidenceStore>,
        identity: AdkIdentity,
    ) -> Self {
        self.journal = Some(Journal { transactions, evidence, identity });
        self
    }

    /// Evaluates `policies` before each payment, as `Ap2Adapter::with_payment_policies` does.
    #[must_use]
    pub fn payment_policies(mut self, policies: PaymentPolicySet) -> Self {
        self.policies = Some(policies);
        self
    }

    /// Records consumed payment mandates in `ledger`; share a durable one between instances
    /// so a mandate is never executed twice. In memory by default.
    #[must_use]
    pub fn mandate_ledger(mut self, ledger: Arc<dyn PaymentMandateLedger>) -> Self {
        self.mandates = Some(ledger);
        self
    }

    /// The payment method the carts accept; `CARD` by default.
    #[must_use]
    pub fn method(mut self, method: impl Into<String>) -> Self {
        self.method = method.into();
        self
    }

    /// Builds the desk.
    ///
    /// # Errors
    ///
    /// A configuration error naming the missing processor, buyer verifier or journal.
    pub fn build(self) -> Result<Ap2Desk> {
        let missing = |what: &str| {
            AdkError::config(format!(
                "the AP2 desk needs {what}; set it on `Ap2Desk::builder` before `build`"
            ))
        };
        let processor = self.processor.ok_or_else(|| missing("the owner's payment processor"))?;
        let buyers =
            self.buyers.ok_or_else(|| missing("a verifier of buyers' user authorisations"))?;
        let Journal { transactions, evidence, identity: journal } =
            self.journal.ok_or_else(|| missing("a transaction and evidence journal"))?;
        let carts = SiteCartVerifier::new(&self.merchant, self.key.kid(), self.key.public_key());
        let checkout = Arc::new(OrderCheckout { transactions: transactions.clone() });
        let mut adapter = Ap2Adapter::new(checkout, processor, transactions.clone(), evidence)
            .with_merchant_authorization_verifier(Arc::new(carts))
            .with_user_authorization_verifier(buyers);
        if let Some(policies) = self.policies {
            adapter = adapter.with_payment_policies(policies);
        }
        if let Some(ledger) = self.mandates {
            adapter = adapter.with_payment_mandate_ledger(ledger);
        }
        Ok(Ap2Desk {
            merchant: MerchantRef {
                merchant_id: self.merchant.clone(),
                legal_name: self.merchant.clone(),
                display_name: Some(self.merchant),
                statement_descriptor: None,
                country_code: None,
                website: None,
                extensions: ProtocolExtensions::default(),
            },
            key: self.key,
            adapter,
            transactions,
            journal,
            method: self.method,
        })
    }
}

impl Ap2Desk {
    /// A desk for the merchant `merchant`, the name the site's carts carry, signing carts and
    /// receipts with `key`, the edge's delegated `receipt` key.
    pub fn builder(merchant: impl Into<String>, key: Arc<dyn DocumentSigner>) -> Ap2DeskBuilder {
        Ap2DeskBuilder {
            merchant: merchant.into(),
            key,
            processor: None,
            buyers: None,
            journal: None,
            policies: None,
            mandates: None,
            method: "CARD".to_string(),
        }
    }

    /// The site's AP2 cart for `order`: its lines, total and currency, payable until the
    /// quote's price ends (A-I25), signed by the desk's key as the merchant's authorisation.
    /// The same order gives the same cart.
    ///
    /// # Errors
    ///
    /// When the key cannot sign.
    pub async fn cart_mandate(
        &self,
        order: &Order,
    ) -> std::result::Result<CartMandate, CapabilityError> {
        let quote = &order.quote;
        let currency = quote.currency.as_str();
        let display_items = quote
            .lines
            .iter()
            .map(|line| {
                let label = line.description.clone().unwrap_or_else(|| line.sku.clone());
                let total = line.unit_amount.saturating_mul(u64::from(line.quantity));
                item(format!("{label} × {}", line.quantity), currency, total)
            })
            .collect();
        let contents = CartContents {
            id: order.id.clone(),
            user_cart_confirmation_required: false,
            payment_request: Ap2PaymentRequest {
                method_data: vec![PaymentMethodData {
                    supported_methods: self.method.clone(),
                    data: None,
                }],
                details: PaymentDetailsInit {
                    id: order.id.clone(),
                    display_items,
                    shipping_options: None,
                    modifiers: None,
                    total: item("Total".to_string(), currency, quote.total),
                },
                options: None,
                shipping_address: None,
            },
            cart_expiry: quote.valid_until.to_rfc3339(),
            merchant_name: self.merchant.legal_name.clone(),
        };
        let proof = sign(&contents, self.key.as_ref()).await.map_err(|error| {
            CapabilityError::Failed(format!("the cart was not signed: {error}"))
        })?;
        Ok(CartMandate { contents, merchant_authorization: Some(proof.jws), timestamp: None })
    }

    fn context(&self, order: &Order, actor: CommerceActor, key: Option<&str>) -> CommerceContext {
        let extensions = match key {
            Some(key) => ProtocolExtensions::from(vec![
                ProtocolExtensionEnvelope::new(protocol())
                    .with_field(IDEMPOTENCY_KEY_FIELD, json!(key)),
            ]),
            None => ProtocolExtensions::default(),
        };
        CommerceContext {
            transaction_id: TransactionId::from(order.id.as_str()),
            session_identity: Some(self.journal.clone()),
            actor,
            merchant_of_record: self.merchant.clone(),
            payment_processor: None,
            mode: CommerceMode::HumanPresent,
            protocol: protocol(),
            extensions,
        }
    }

    fn actor(id: String, role: CommerceActorRole) -> CommerceActor {
        CommerceActor {
            actor_id: id,
            role,
            display_name: None,
            tenant_id: None,
            extensions: ProtocolExtensions::default(),
        }
    }

    /// Opens `order`'s AP2 transaction with `cart`, unless it is open already.
    async fn open(
        &self,
        order: &Order,
        cart: CartMandate,
    ) -> std::result::Result<(), CapabilityError> {
        let lookup = TransactionLookup {
            transaction_id: TransactionId::from(order.id.as_str()),
            session_identity: Some(self.journal.clone()),
        };
        if self.transactions.get(lookup).await.map_err(answered)?.is_some() {
            return Ok(());
        }
        let merchant = Self::actor(self.merchant.merchant_id.clone(), CommerceActorRole::Merchant);
        self.adapter
            .submit_cart_mandate(self.context(order, merchant, None), cart)
            .await
            .map_err(answered)?;
        Ok(())
    }
}

#[async_trait]
impl PaymentDesk for Ap2Desk {
    async fn take(&self, call: &CapabilityCall) -> std::result::Result<Value, CapabilityError> {
        Err(refused(format!(
            "{} is paid through an order: place one, then pay it with `pay_order`",
            call.capability
        )))
    }

    async fn take_for(
        &self,
        call: &CapabilityCall,
        order: &Order,
    ) -> std::result::Result<Value, CapabilityError> {
        let Some(mandate) = call.input.get("mandate") else {
            return Err(refused(format!(
                "an AP2 payment carries the buyer's payment mandate for the order's cart as \
                 `mandate`; the cart came with order {}",
                order.id
            )));
        };
        let mandate: PaymentMandate = serde_json::from_value(mandate.clone())
            .map_err(|error| refused(format!("the payment mandate does not read: {error}")))?;
        self.open(order, self.cart_mandate(order).await?).await?;
        let buyer =
            call.requester.as_ref().map_or_else(|| "buyer".to_string(), ToString::to_string);
        let context = self.context(
            order,
            Self::actor(buyer, CommerceActorRole::Shopper),
            Some(&call.idempotency_key),
        );
        let mandate_id = mandate.payment_mandate_contents.payment_mandate_id.clone();
        let result =
            self.adapter.submit_payment_mandate(context, mandate).await.map_err(answered)?;
        match result.outcome {
            PaymentExecutionOutcome::Authorized | PaymentExecutionOutcome::Completed => {}
            PaymentExecutionOutcome::InterventionRequired => {
                return Err(refused(
                    "the buyer's payment provider asks the buyer to confirm the payment; \
                     nothing was paid",
                ));
            }
            PaymentExecutionOutcome::Failed => {
                return Err(refused(
                    "the payment processor declined the payment; nothing was paid",
                ));
            }
        }
        let quote = &order.quote;
        let mut details =
            ReceiptDetails::new(&call.capability, &order.id, quote.total, &quote.currency)
                .with_rail(AP2_RAIL)
                .with_lines(quote.lines.clone())
                .settling_quote(quote.id, quote.release_seq);
        if let Some(requester) = &call.requester {
            details = details.for_requester(requester.clone());
        }
        // The money moved: the order is paid even when its receipt cannot be signed.
        let receipt = match issue_receipt(&ReceiptKey::new(self.key.clone()), details, Utc::now())
            .await
        {
            Ok(receipt) => Some(receipt),
            Err(error) => {
                tracing::error!(error = %error, order.id = %order.id, "a paid order's receipt was not signed");
                None
            }
        };
        Ok(json!({
            "status": PAID,
            "orderId": order.id,
            "rail": AP2_RAIL,
            "receipt": receipt,
            "ap2": {
                "paymentMandateId": mandate_id,
                "transactionId": result.transaction.transaction_id,
                "outcome": result.outcome,
            },
        }))
    }

    async fn checkout(&self, order: &Order) -> std::result::Result<Option<Value>, CapabilityError> {
        let cart = self.cart_mandate(order).await?;
        self.open(order, cart.clone()).await?;
        Ok(Some(json!({ "rail": AP2_RAIL, "cartMandate": cart })))
    }
}

/// Verifies that a cart carries the site's signature as its merchant authorisation: a
/// detached JWS by the site's key over the cart's contents (spec 3.5 §7.5). The adapter
/// behind an [`Ap2Desk`] checks every cart with it, and a buyer can check a cart with the
/// site's published `receipt` key.
///
/// # Example
///
/// ```rust
/// use adk_awp::signing::jws::{DocumentSigner, Ed25519DocumentSigner};
/// use adk_website::edge::ap2::SiteCartVerifier;
///
/// let key = Ed25519DocumentSigner::from_seed("receipt-2026-10", [7; 32]).unwrap();
/// let verifier = SiteCartVerifier::new("Juniper Bakery", key.kid(), key.public_key());
/// # let _ = verifier;
/// ```
#[derive(Debug, Clone)]
pub struct SiteCartVerifier {
    merchant: String,
    kid: String,
    key: PublicKeyJwk,
}

impl SiteCartVerifier {
    /// Verifies carts of `merchant` signed by the key `kid` whose public key is `key`.
    pub fn new(merchant: impl Into<String>, kid: impl Into<String>, key: PublicKeyJwk) -> Self {
        Self { merchant: merchant.into(), kid: kid.into(), key }
    }
}

#[async_trait]
impl MerchantAuthorizationVerifier for SiteCartVerifier {
    async fn verify_cart_authorization(
        &self,
        mandate: &CartMandate,
        artifact: &AuthorizationArtifact,
    ) -> Result<VerifiedAuthorization> {
        let unverified = |reason: String| {
            AdkError::new(
                ErrorComponent::Auth,
                ErrorCategory::Unauthorized,
                "website.ap2.cart_unverified",
                format!("the cart's merchant authorisation does not verify: {reason}"),
            )
        };
        if !mandate.contents.merchant_name.eq_ignore_ascii_case(&self.merchant) {
            return Err(unverified(format!(
                "it names {}, not {}",
                mandate.contents.merchant_name, self.merchant
            )));
        }
        let proof = Proof::eddsa(&self.kid, &artifact.value);
        verify(&mandate.contents, &proof, &self.key)
            .map_err(|error| unverified(error.to_string()))?;
        let mut claims = Map::new();
        claims.insert(VERIFIED_MERCHANT_NAME_CLAIM.to_string(), json!(self.merchant));
        claims.insert(CRYPTOGRAPHICALLY_VERIFIED_CLAIM.to_string(), json!(true));
        Ok(VerifiedAuthorization::new("merchant_authorization", claims))
    }
}

/// The adapter's checkout service: each AP2 cart opens or updates its transaction in the
/// journal.
struct OrderCheckout {
    transactions: Arc<dyn TransactionStore>,
}

#[async_trait]
impl MerchantCheckoutService for OrderCheckout {
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
            transaction_id: command.context.transaction_id.clone(),
            session_identity: command.context.session_identity,
        };
        let mut record = self.transactions.get(lookup).await?.ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::NotFound,
                "website.ap2.no_transaction",
                format!("no AP2 transaction {}", command.context.transaction_id.as_str()),
            )
        })?;
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
        Err(unsupported("complete checkouts"))
    }

    async fn cancel_checkout(&self, _: CancelCheckoutCommand) -> Result<TransactionRecord> {
        Err(unsupported("cancel checkouts"))
    }

    async fn apply_order_update(&self, _: OrderUpdateCommand) -> Result<TransactionRecord> {
        Err(unsupported("apply order updates"))
    }
}

/// A payment processor for tests and sandboxes (spec 3.5 S9): authorises every payment
/// mandate the adapter lets through and records it. It moves no money; never give it to a
/// site that takes real payments.
///
/// # Example
///
/// ```rust,ignore
/// let processor = Arc::new(SandboxProcessor::new(transactions.clone()));
/// let desk = Ap2Desk::builder("Juniper Bakery", key).processor(processor.clone());
/// ```
pub struct SandboxProcessor {
    transactions: Arc<dyn TransactionStore>,
    executed: std::sync::Mutex<Vec<String>>,
}

impl SandboxProcessor {
    /// A processor over the desk's `transactions`.
    pub fn new(transactions: Arc<dyn TransactionStore>) -> Self {
        Self { transactions, executed: std::sync::Mutex::new(Vec::new()) }
    }

    /// The transactions it executed a payment for, in order: one per payment.
    pub fn executed(&self) -> Vec<String> {
        self.executed.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

#[async_trait]
impl PaymentExecutionService for SandboxProcessor {
    async fn execute_payment(
        &self,
        command: ExecutePaymentCommand,
    ) -> Result<PaymentExecutionResult> {
        let lookup = TransactionLookup {
            transaction_id: command.context.transaction_id.clone(),
            session_identity: command.context.session_identity.clone(),
        };
        let mut record = self.transactions.get(lookup).await?.ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::NotFound,
                "website.ap2.no_transaction",
                format!("no AP2 transaction {}", command.context.transaction_id.as_str()),
            )
        })?;
        if record.state.can_transition_to(&TransactionState::Authorized) {
            record
                .transition_to(TransactionState::Authorized, Utc::now())
                .map_err(AdkError::from)?;
        }
        record.recompute_safe_summary();
        self.transactions.upsert(record.clone()).await?;
        self.executed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(command.context.transaction_id.as_str().to_string());
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
        command: SyncPaymentOutcomeCommand,
    ) -> Result<TransactionRecord> {
        let lookup = TransactionLookup {
            transaction_id: command.context.transaction_id.clone(),
            session_identity: command.context.session_identity,
        };
        self.transactions.get(lookup).await?.ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::NotFound,
                "website.ap2.no_transaction",
                format!("no AP2 transaction {}", command.context.transaction_id.as_str()),
            )
        })
    }
}

/// An AP2 journal in memory, for tests and sandboxes: transactions in an in-memory session
/// `identity` names, evidence in an in-memory artifact service. A served site keeps them in
/// its own session and artifact services.
///
/// # Errors
///
/// When the session cannot be created.
pub async fn in_memory_journal(
    identity: &AdkIdentity,
) -> Result<(Arc<dyn TransactionStore>, Arc<dyn EvidenceStore>)> {
    use adk_session::{CreateRequest, InMemorySessionService, SessionService};

    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: identity.app_name.as_ref().to_string(),
            user_id: identity.user_id.as_ref().to_string(),
            session_id: Some(identity.session_id.as_ref().to_string()),
            state: std::collections::HashMap::new(),
        })
        .await?;
    let transactions: Arc<dyn TransactionStore> =
        Arc::new(adk_payments::journal::SessionBackedTransactionStore::new(sessions));
    let evidence: Arc<dyn EvidenceStore> =
        Arc::new(adk_payments::journal::ArtifactBackedEvidenceStore::new(Arc::new(
            adk_artifact::InMemoryArtifactService::new(),
        )));
    Ok((transactions, evidence))
}
