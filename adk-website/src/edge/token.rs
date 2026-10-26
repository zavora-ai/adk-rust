//! Token rails (spec 3.5 §7.5 "Commerce", §7.14, S9): an order paid with the single-use
//! payment token the buyer's payment provider issued for it, `pay_order`'s `credential`.
//!
//! | Part | Role |
//! |---|---|
//! | [`TokenDesk`] | The [`PaymentDesk`] of one or more token rails, such as `card`: hands the buyer's token to the owner's payment processor, once per order, and answers with a receipt signed by the edge's `receipt` key |
//! | [`PaymentRails`] | One desk per rail behind the edge's single payment desk, so a site takes AP2 mandates and token rails side by side |
//!
//! The owner's payment processor is an `adk-payments` `PaymentExecutionService` that
//! redeems the token with the provider that issued it; the site holds no custody and keeps
//! no token: the token reaches the processor only, never the journal, the answer or a log.

use std::collections::BTreeSet;
use std::sync::Arc;

use adk_awp::signing::jws::DocumentSigner;
use adk_awp::v2::capability::{CapabilityCall, CapabilityError};
use adk_core::identity::AdkIdentity;
use adk_core::{AdkError, ErrorCategory, Result};
use adk_payments::domain::{
    Cart, CartLine, CommerceActor, CommerceActorRole, CommerceMode, MerchantRef, Money,
    PaymentMethodSelection, ProtocolDescriptor, ProtocolExtensionEnvelope, ProtocolExtensions,
    TransactionId, TransactionRecord,
};
use adk_payments::kernel::{
    CommerceContext, ExecutePaymentCommand, PaymentExecutionOutcome, PaymentExecutionService,
    TransactionLookup, TransactionStore,
};
use adk_payments::wallet::{ReceiptDetails, issue_receipt};
use async_trait::async_trait;
use awp_types::ErrorCode;
use chrono::Utc;
use serde_json::{Value, json};

use super::adapters::ReceiptKey;
use super::handlers::{Order, PAID, PaymentDesk};

/// The protocol name a token payment carries to the processor.
pub const TOKEN_PROTOCOL: &str = "awp-token";

/// The extension field the processor reads as its deduplication key.
const IDEMPOTENCY_KEY_FIELD: &str = "idempotency_key";

/// The payment-method kind of a token rail's selection.
const TOKEN_SELECTION: &str = "single_use_token";

fn refused(message: impl Into<String>) -> CapabilityError {
    CapabilityError::refused(ErrorCode::PaymentPolicyRejected, message)
}

/// A processor or store error as the buyer sees it: a refusal for anything the buyer or the
/// token caused, an internal error for an unavailable or failing backend.
fn answered(error: AdkError) -> CapabilityError {
    match error.category {
        ErrorCategory::Unavailable | ErrorCategory::Timeout | ErrorCategory::Internal => {
            CapabilityError::Failed(error.to_string())
        }
        _ => refused(error.message),
    }
}

fn protocol() -> ProtocolDescriptor {
    ProtocolDescriptor::new(TOKEN_PROTOCOL, Some("2.0".to_string()))
}

/// `minor` units of `currency` as money.
fn money(currency: &str, minor: u64) -> std::result::Result<Money, CapabilityError> {
    let minor = i64::try_from(minor)
        .map_err(|_| refused(format!("{minor} {currency} is more than a payment carries")))?;
    Ok(Money::new(currency, minor, Money::iso_minor_unit_scale(currency).unwrap_or(2)))
}

/// Takes payment for the site's orders on token rails (spec 3.5 §7.5 "Commerce", §7.14, S9).
///
/// `pay_order` names the order, the rail and, as `credential`, the single-use payment token
/// the buyer's payment provider issued for the order's total. The desk opens the order's
/// transaction in its journal, hands the token to the owner's payment processor with the
/// order's total and the call's idempotency key, and answers `{status: "paid", orderId,
/// rail, receipt, transactionId, outcome}`, the receipt signed by the edge's `receipt` key.
/// A call on another rail, or without a token, is refused `payment_policy_rejected`; so is
/// a token the processor declines, and nothing is paid. The edge pays an order once
/// whatever the keys of its calls.
///
/// # Example
///
/// ```rust,ignore
/// let (transactions, _) = in_memory_journal(&identity).await?;
/// let desk = TokenDesk::builder("Juniper Bakery", receipt_key)
///     .rail("card")
///     .processor(owner_processor)
///     .journal(transactions, identity)
///     .build()?;
/// let parts = SiteEdgeParts { payments: Some(Arc::new(desk)), ..parts };
/// ```
pub struct TokenDesk {
    merchant: MerchantRef,
    key: Arc<dyn DocumentSigner>,
    rails: BTreeSet<String>,
    processor: Arc<dyn PaymentExecutionService>,
    transactions: Arc<dyn TransactionStore>,
    journal: AdkIdentity,
}

impl std::fmt::Debug for TokenDesk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenDesk")
            .field("merchant", &self.merchant.legal_name)
            .field("rails", &self.rails)
            .finish_non_exhaustive()
    }
}

/// Builds a [`TokenDesk`].
pub struct TokenDeskBuilder {
    merchant: String,
    key: Arc<dyn DocumentSigner>,
    rails: BTreeSet<String>,
    processor: Option<Arc<dyn PaymentExecutionService>>,
    journal: Option<(Arc<dyn TransactionStore>, AdkIdentity)>,
}

impl TokenDeskBuilder {
    /// Takes tokens on `rail`, the name `pay_order` gives, such as `card`. At least one.
    #[must_use]
    pub fn rail(mut self, rail: impl Into<String>) -> Self {
        self.rails.insert(rail.into());
        self
    }

    /// The owner's payment processor, which redeems each token with the provider that
    /// issued it. Required.
    #[must_use]
    pub fn processor(mut self, processor: Arc<dyn PaymentExecutionService>) -> Self {
        self.processor = Some(processor);
        self
    }

    /// Keeps the payments' transactions in `transactions` under the session `identity`.
    /// Required; `ap2::in_memory_journal` gives an in-memory one.
    #[must_use]
    pub fn journal(
        mut self,
        transactions: Arc<dyn TransactionStore>,
        identity: AdkIdentity,
    ) -> Self {
        self.journal = Some((transactions, identity));
        self
    }

    /// Builds the desk.
    ///
    /// # Errors
    ///
    /// A configuration error naming the missing rail, processor or journal.
    pub fn build(self) -> Result<TokenDesk> {
        let missing = |what: &str| {
            AdkError::config(format!(
                "the token desk needs {what}; set it on `TokenDesk::builder` before `build`"
            ))
        };
        if self.rails.is_empty() {
            return Err(missing("a rail, such as `card`"));
        }
        let processor = self.processor.ok_or_else(|| missing("the owner's payment processor"))?;
        let (transactions, journal) =
            self.journal.ok_or_else(|| missing("a transaction journal"))?;
        Ok(TokenDesk {
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
            rails: self.rails,
            processor,
            transactions,
            journal,
        })
    }
}

impl TokenDesk {
    /// A desk for the merchant `merchant`, signing receipts with `key`, the edge's delegated
    /// `receipt` key.
    pub fn builder(merchant: impl Into<String>, key: Arc<dyn DocumentSigner>) -> TokenDeskBuilder {
        TokenDeskBuilder {
            merchant: merchant.into(),
            key,
            rails: BTreeSet::new(),
            processor: None,
            journal: None,
        }
    }

    /// The rails it takes.
    pub fn rails(&self) -> &BTreeSet<String> {
        &self.rails
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

    /// The order's quote as a cart.
    fn cart(order: &Order) -> std::result::Result<Cart, CapabilityError> {
        let quote = &order.quote;
        let mut lines = Vec::with_capacity(quote.lines.len());
        for (n, line) in quote.lines.iter().enumerate() {
            let total = line.unit_amount.saturating_mul(u64::from(line.quantity));
            lines.push(CartLine {
                line_id: format!("{}-{n}", order.id),
                merchant_sku: Some(line.sku.clone()),
                title: line.description.clone().unwrap_or_else(|| line.sku.clone()),
                quantity: line.quantity,
                unit_price: money(&quote.currency, line.unit_amount)?,
                total_price: money(&quote.currency, total)?,
                product_class: None,
                extensions: ProtocolExtensions::default(),
            });
        }
        Ok(Cart {
            cart_id: Some(order.id.clone()),
            lines,
            subtotal: None,
            adjustments: Vec::new(),
            total: money(&quote.currency, quote.total)?,
            affiliate_attribution: None,
            extensions: ProtocolExtensions::default(),
        })
    }

    /// Opens `order`'s transaction, unless it is open already.
    async fn open(
        &self,
        order: &Order,
        buyer: &CommerceActor,
    ) -> std::result::Result<(), CapabilityError> {
        let lookup = TransactionLookup {
            transaction_id: TransactionId::from(order.id.as_str()),
            session_identity: Some(self.journal.clone()),
        };
        if self.transactions.get(lookup).await.map_err(answered)?.is_some() {
            return Ok(());
        }
        let mut record = TransactionRecord::new(
            TransactionId::from(order.id.as_str()),
            buyer.clone(),
            self.merchant.clone(),
            CommerceMode::HumanPresent,
            Self::cart(order)?,
            Utc::now(),
        );
        record.session_identity = Some(self.journal.clone());
        record.recompute_safe_summary();
        self.transactions.upsert(record).await.map_err(answered)
    }
}

#[async_trait]
impl PaymentDesk for TokenDesk {
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
        let rail = call.input["rail"].as_str().unwrap_or_default();
        if !self.rails.contains(rail) {
            let rails: Vec<&str> = self.rails.iter().map(String::as_str).collect();
            return Err(refused(format!(
                "this desk takes {} payments, not `{rail}`",
                rails.join(", ")
            )));
        }
        let Some(token) = call.input["credential"].as_str().filter(|t| !t.trim().is_empty()) else {
            return Err(refused(format!(
                "a `{rail}` payment carries the single-use token the buyer's payment provider \
                 issued for order {} as `credential`",
                order.id
            )));
        };
        let buyer = Self::actor(
            call.requester.as_ref().map_or_else(|| "buyer".to_string(), ToString::to_string),
            CommerceActorRole::Shopper,
        );
        self.open(order, &buyer).await?;
        let quote = &order.quote;
        let envelope = ProtocolExtensions::from(vec![
            ProtocolExtensionEnvelope::new(protocol())
                .with_field(IDEMPOTENCY_KEY_FIELD, json!(call.idempotency_key))
                .with_field("rail", json!(rail)),
        ]);
        let command = ExecutePaymentCommand {
            context: CommerceContext {
                transaction_id: TransactionId::from(order.id.as_str()),
                session_identity: Some(self.journal.clone()),
                actor: buyer,
                merchant_of_record: self.merchant.clone(),
                payment_processor: None,
                mode: CommerceMode::HumanPresent,
                protocol: protocol(),
                extensions: envelope.clone(),
            },
            amount: money(&quote.currency, quote.total)?,
            selected_payment_method: Some(PaymentMethodSelection {
                selection_kind: TOKEN_SELECTION.to_string(),
                reference: Some(token.to_string()),
                display_hint: Some(rail.to_string()),
                extensions: ProtocolExtensions::default(),
            }),
            supporting_evidence_refs: Vec::new(),
            extensions: envelope,
        };
        let result = self.processor.execute_payment(command).await.map_err(answered)?;
        match result.outcome {
            PaymentExecutionOutcome::Authorized | PaymentExecutionOutcome::Completed => {}
            PaymentExecutionOutcome::InterventionRequired => {
                return Err(refused(
                    "the buyer's payment provider asks the buyer to confirm the payment; \
                     nothing was paid",
                ));
            }
            PaymentExecutionOutcome::Failed => {
                return Err(refused("the payment processor declined the token; nothing was paid"));
            }
        }
        let mut details =
            ReceiptDetails::new(&call.capability, &order.id, quote.total, &quote.currency)
                .with_rail(rail)
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
            "rail": rail,
            "receipt": receipt,
            "transactionId": result.transaction.transaction_id,
            "outcome": result.outcome,
        }))
    }
}

/// The site's payment desks by rail (spec 3.5 §7.5 "Commerce"): `pay_order`'s `rail` picks
/// the desk, so an AP2 desk and token rails serve one site side by side.
///
/// A call naming no rail goes to the first desk added. `place_order` gathers what each desk
/// returns for the order: one desk's answer as it is, several as `{rails: [...]}`.
///
/// # Example
///
/// ```rust,ignore
/// let rails = PaymentRails::new()
///     .rail(AP2_RAIL, Arc::new(ap2_desk))
///     .rail("card", Arc::new(token_desk));
/// let parts = SiteEdgeParts { payments: Some(Arc::new(rails)), ..parts };
/// ```
#[derive(Default)]
pub struct PaymentRails {
    desks: Vec<(String, Arc<dyn PaymentDesk>)>,
}

impl std::fmt::Debug for PaymentRails {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.desks.iter().map(|(rail, _)| rail)).finish()
    }
}

impl PaymentRails {
    /// No rails.
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes payments on `rail` through `desk`; a desk may serve several rails.
    #[must_use]
    pub fn rail(mut self, rail: impl Into<String>, desk: Arc<dyn PaymentDesk>) -> Self {
        self.desks.push((rail.into(), desk));
        self
    }

    fn desk(
        &self,
        call: &CapabilityCall,
    ) -> std::result::Result<&dyn PaymentDesk, CapabilityError> {
        let first = || {
            self.desks.first().map(|(_, desk)| desk.as_ref()).ok_or_else(|| {
                refused("the site takes no payments until its owner connects a payment rail")
            })
        };
        let Some(rail) = call.input.get("rail").and_then(Value::as_str) else {
            return first();
        };
        self.desks.iter().find(|(name, _)| name == rail).map(|(_, desk)| desk.as_ref()).ok_or_else(
            || {
                let rails: Vec<&str> = self.desks.iter().map(|(name, _)| name.as_str()).collect();
                refused(format!("the site takes payments on {}, not `{rail}`", rails.join(", ")))
            },
        )
    }
}

#[async_trait]
impl PaymentDesk for PaymentRails {
    async fn take(&self, call: &CapabilityCall) -> std::result::Result<Value, CapabilityError> {
        self.desk(call)?.take(call).await
    }

    async fn take_for(
        &self,
        call: &CapabilityCall,
        order: &Order,
    ) -> std::result::Result<Value, CapabilityError> {
        self.desk(call)?.take_for(call, order).await
    }

    async fn checkout(&self, order: &Order) -> std::result::Result<Option<Value>, CapabilityError> {
        let mut answers = Vec::new();
        let mut seen: Vec<usize> = Vec::new();
        for (_, desk) in &self.desks {
            // A desk serving several rails prepares the order once.
            let address = Arc::as_ptr(desk).cast::<()>() as usize;
            if seen.contains(&address) {
                continue;
            }
            seen.push(address);
            if let Some(answer) = desk.checkout(order).await? {
                answers.push(answer);
            }
        }
        Ok(match answers.len() {
            0 => None,
            1 => answers.pop(),
            _ => Some(json!({ "rails": answers })),
        })
    }
}
