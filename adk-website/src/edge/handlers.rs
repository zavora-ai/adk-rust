//! The capability packs' handlers on the agent edge (spec 3.5 §7.16 `packs`, §7.4, §7.9,
//! §7.10, A-I7, A-I8, A-I25, A-I26).
//!
//! | Handler | Capabilities | What it does |
//! |---|---|---|
//! | [`FormHandler`] | `submit_inquiry`, `subscribe_newsletter`, `request_prospectus`, `licensing_enquiry`, `volunteer`, `register`, `submit_service_form` | Keeps the contact details in the [`ContactBook`], the lead in the edge's `LeadStore`, and files one typed `lead_captured` signal |
//! | [`QuoteHandler`] | `get_quote` | Quotes products at the prices of the signed release; the quote keeps its price until it expires (A-I25, A-I26) |
//! | [`OrderHandler`] | `place_order`, `order_status`, `request_return` | Orders against an open quote, each with what the [`PaymentDesk`] needs to take its payment; a return is held for the owner by its declaration |
//! | [`PaymentHandler`] | `pay_order`, `donate`, `subscribe`, `buy_ticket` | Money through the owner's [`PaymentDesk`]; refused while the site has none; an order is paid once |
//! | [`BookingHandler`] | `check_availability`, `book`, `book_appointment` | Slots of a [`Calendar`]; a taken slot is `inventory_unavailable` |
//! | [`SearchHandler`] | `search_site` | The release's index |
//! | [`ListingHandler`] | `list_products`, `list_notices`, `list_events`, `get_schedule`, `practice_information` | Published items of the release, read-only |
//! | [`StatusHandler`] | `lookup_status` | An application's status by its reference |
//!
//! The governed path — tiers, schemas, idempotency keys, consent receipts, holds — runs in the
//! edge before a handler is called; a handler sees only a validated call. The handlers of
//! `place_order`, `pay_order`, `book` and `book_appointment` report each call's amount — the
//! quote's total, the order's total, the slot's price — so an owner's amount threshold
//! ([`ApprovalThresholds`](crate::packs::ApprovalThresholds)) holds a large call for the owner
//! (`ApprovalHint::AboveAmount`, spec 3.5 §7.4, S9).

use std::collections::BTreeMap;
use std::sync::{Arc, PoisonError};

use adk_awp::v2::capability::{CapabilityCall, CapabilityError, CapabilityHandler};
use adk_awp::v2::store::{Lead, LeadStore};
use adk_harness::website::SiteRuntime;
use adk_server::awp::{SignalEmitter, SiteSearch};
use async_trait::async_trait;
use awp_types::{
    Bounded, Channel, ConsentReceiptRef, ContactRef, ContentItem, ContentKind, ContentState,
    ErrorCode, LeadRef, LineItem, QuoteDocument, Signal, SignalKind,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use uuid::Uuid;

/// Characters a signal's answer keeps (spec 3.5 §7.10 "Bounded").
const ANSWER_MAX_CHARS: usize = 280;

fn failed(error: impl std::fmt::Display) -> CapabilityError {
    CapabilityError::Failed(error.to_string())
}

fn refused(code: ErrorCode, message: impl Into<String>) -> CapabilityError {
    CapabilityError::Refused { code, message: message.into() }
}

/// The published content the read-only capabilities answer from: the site's working copy
/// as last released (spec 3.5 A-I26).
#[derive(Clone)]
pub struct PublishedContent {
    runtime: Arc<SiteRuntime>,
}

impl PublishedContent {
    /// The content of `runtime`'s site.
    pub fn new(runtime: Arc<SiteRuntime>) -> Self {
        Self { runtime }
    }

    /// The published items of `kind`.
    ///
    /// # Errors
    ///
    /// The content store's error.
    pub async fn items(&self, kind: ContentKind) -> Result<Vec<ContentItem>, CapabilityError> {
        Ok(self
            .runtime
            .store()
            .list()
            .await
            .map_err(failed)?
            .into_iter()
            .filter(|item| item.kind == kind && item.state == ContentState::Published)
            .collect())
    }

    /// The release the host serves.
    pub async fn release_seq(&self) -> u64 {
        self.runtime.pipeline().previous().await.map_or(0, |previous| previous.manifest.seq)
    }
}

/// A person's contact details, kept apart from the lead, which holds only a reference
/// (spec 3.5 §7.10, A-I3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contact {
    /// The reference leads carry.
    pub contact_ref: ContactRef,
    /// The email address.
    pub email: Option<String>,
    /// The name given.
    pub name: Option<String>,
    /// The consent receipts given with it.
    pub consent: Vec<Uuid>,
    /// When it was given.
    pub at: DateTime<Utc>,
}

/// The site's contact book: where the back office reaches the people who wrote (spec 3.5
/// §7.10). Implementations keep it in the site's own store.
#[async_trait]
pub trait ContactBook: Send + Sync {
    /// Keeps `contact`, replacing an earlier one with its reference.
    ///
    /// # Errors
    ///
    /// The book's error.
    async fn put(&self, contact: Contact) -> adk_core::Result<()>;

    /// The contacts given with any of `receipts`.
    ///
    /// # Errors
    ///
    /// The book's error.
    async fn by_receipts(&self, receipts: &[Uuid]) -> adk_core::Result<Vec<Contact>>;

    /// Erases the contacts given with any of `receipts`; returns how many.
    ///
    /// # Errors
    ///
    /// The book's error.
    async fn erase(&self, receipts: &[Uuid]) -> adk_core::Result<usize>;
}

/// A [`ContactBook`] in memory.
#[derive(Debug, Default)]
pub struct InMemoryContactBook {
    contacts: Mutex<BTreeMap<String, Contact>>,
}

#[async_trait]
impl ContactBook for InMemoryContactBook {
    async fn put(&self, contact: Contact) -> adk_core::Result<()> {
        self.contacts.lock().await.insert(contact.contact_ref.0.clone(), contact);
        Ok(())
    }

    async fn by_receipts(&self, receipts: &[Uuid]) -> adk_core::Result<Vec<Contact>> {
        Ok(self
            .contacts
            .lock()
            .await
            .values()
            .filter(|c| c.consent.iter().any(|r| receipts.contains(r)))
            .cloned()
            .collect())
    }

    async fn erase(&self, receipts: &[Uuid]) -> adk_core::Result<usize> {
        let mut contacts = self.contacts.lock().await;
        let before = contacts.len();
        contacts.retain(|_, c| !c.consent.iter().any(|r| receipts.contains(r)));
        Ok(before - contacts.len())
    }
}

/// The reference of `email`: a digest, so a lead and a signal never carry the address.
fn contact_ref(email: Option<&str>, key: &str) -> ContactRef {
    let basis = email.map_or_else(|| key.to_string(), |e| e.trim().to_ascii_lowercase());
    let digest = adk_site::release::file_digest(basis.as_bytes()).to_string();
    ContactRef(format!("contact:{}", digest.trim_start_matches("sha256:").get(..16).unwrap_or("")))
}

/// Forms: enquiries, sign-ups and applications (spec 3.5 §7.16 `contact`, §7.10 signals).
pub struct FormHandler {
    leads: Arc<dyn LeadStore>,
    contacts: Arc<dyn ContactBook>,
    signals: Arc<dyn SignalEmitter>,
    emitted_by: String,
}

impl FormHandler {
    /// A handler keeping leads in `leads` and contacts in `contacts`, signalling through
    /// `signals` as the site's agent `emitted_by`.
    pub fn new(
        leads: Arc<dyn LeadStore>,
        contacts: Arc<dyn ContactBook>,
        signals: Arc<dyn SignalEmitter>,
        emitted_by: impl Into<String>,
    ) -> Self {
        Self { leads, contacts, signals, emitted_by: emitted_by.into() }
    }
}

#[async_trait]
impl CapabilityHandler for FormHandler {
    async fn handle(&self, call: CapabilityCall) -> Result<Value, CapabilityError> {
        let fields = call.input.as_object().cloned().unwrap_or_default();
        let text = |key: &str| fields.get(key).and_then(Value::as_str).map(str::to_string);
        let email = text("email");
        let contact_ref = contact_ref(email.as_deref(), &call.idempotency_key);
        let now = Utc::now();
        self.contacts
            .put(Contact {
                contact_ref: contact_ref.clone(),
                email: email.clone(),
                name: text("name"),
                consent: call.consent.clone(),
                at: now,
            })
            .await
            .map_err(failed)?;
        let answers: BTreeMap<String, Bounded> = fields
            .iter()
            .filter(|(key, _)| !matches!(key.as_str(), "email" | "name"))
            .filter_map(|(key, value)| {
                let text = match value {
                    Value::String(text) => text.clone(),
                    Value::Null => return None,
                    other => other.to_string(),
                };
                Some((key.clone(), Bounded::truncated(&text, ANSWER_MAX_CHARS)))
            })
            .chain(std::iter::once((
                "capability".to_string(),
                Bounded::truncated(&call.capability, ANSWER_MAX_CHARS),
            )))
            .collect();
        let consent: Vec<ConsentReceiptRef> =
            call.consent.iter().map(|id| ConsentReceiptRef(*id)).collect();
        let lead = Lead {
            id: LeadRef(format!("lead-{}", Uuid::new_v4().simple())),
            captured_at: now,
            source: Channel::Http,
            contact_ref,
            consent: consent.clone(),
            answers: answers.clone(),
            thread_ref: None,
            score: None,
        };
        self.leads.insert(lead.clone()).await.map_err(failed)?;
        self.signals
            .emit(Signal {
                id: Uuid::new_v4(),
                kind: SignalKind::LeadCaptured {
                    lead_ref: lead.id.clone(),
                    source: Channel::Http,
                    consent,
                    answers,
                },
                emitted_by: self.emitted_by.clone(),
                emitted_at: now,
            })
            .await
            .map_err(failed)?;
        Ok(json!({ "received": true, "reference": lead.id.0, "receivedAt": now }))
    }
}

/// The quotes the site issued, for orders against them (spec 3.5 A-I25).
#[derive(Debug, Default)]
pub struct QuoteBook {
    // A blocking lock, never held across an await: the edge reads a call's amount through
    // `CapabilityHandler::call_amount`, which is synchronous.
    quotes: std::sync::Mutex<BTreeMap<Uuid, QuoteDocument>>,
}

impl QuoteBook {
    /// The quote `id`.
    pub async fn get(&self, id: &Uuid) -> Option<QuoteDocument> {
        self.quote(id)
    }

    fn quote(&self, id: &Uuid) -> Option<QuoteDocument> {
        self.quotes.lock().unwrap_or_else(PoisonError::into_inner).get(id).cloned()
    }

    fn insert(&self, quote: QuoteDocument) {
        self.quotes.lock().unwrap_or_else(PoisonError::into_inner).insert(quote.id, quote);
    }
}

/// Quotes at the signed release's prices (spec 3.5 §7.16 `quote`, A-I25, A-I26).
pub struct QuoteHandler {
    content: PublishedContent,
    quotes: Arc<QuoteBook>,
    validity: Duration,
}

impl QuoteHandler {
    /// Quotes from `content`, kept in `quotes`, valid for `validity`.
    pub fn new(content: PublishedContent, quotes: Arc<QuoteBook>, validity: Duration) -> Self {
        Self { content, quotes, validity }
    }
}

#[async_trait]
impl CapabilityHandler for QuoteHandler {
    async fn handle(&self, call: CapabilityCall) -> Result<Value, CapabilityError> {
        let products = self.content.items(ContentKind::Product).await?;
        let wanted = call.input["items"].as_array().cloned().unwrap_or_default();
        let mut lines = Vec::new();
        let mut currency: Option<String> = None;
        for line in &wanted {
            let sku = line["sku"].as_str().unwrap_or_default();
            let quantity = line["quantity"].as_u64().unwrap_or(1);
            let product = products
                .iter()
                .find(|item| item.fields.get("sku").and_then(Value::as_str) == Some(sku))
                .ok_or_else(|| {
                    refused(ErrorCode::InventoryUnavailable, format!("no product {sku}"))
                })?;
            let price = product.fields.get("price").and_then(Value::as_u64).ok_or_else(|| {
                refused(ErrorCode::InventoryUnavailable, format!("{sku} has no published price"))
            })?;
            let line_currency =
                product.fields.get("currency").and_then(Value::as_str).unwrap_or("USD").to_string();
            if currency.as_ref().is_some_and(|c| *c != line_currency) {
                return Err(refused(ErrorCode::InvalidRequest, "a quote is in one currency"));
            }
            currency = Some(line_currency);
            lines.push(LineItem {
                sku: sku.to_string(),
                description: product
                    .fields
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                quantity: u32::try_from(quantity).unwrap_or(u32::MAX),
                unit_amount: price,
            });
        }
        let total = lines
            .iter()
            .map(|line| line.unit_amount.saturating_mul(u64::from(line.quantity)))
            .fold(0u64, u64::saturating_add);
        let now = Utc::now();
        let quote = QuoteDocument {
            id: Uuid::new_v4(),
            capability: call.capability.clone(),
            requester: call.requester.clone(),
            lines,
            total,
            currency: currency.unwrap_or_else(|| "USD".into()),
            release_seq: self.content.release_seq().await,
            issued_at: now,
            valid_until: now + self.validity,
            proof: None,
        };
        self.quotes.insert(quote.clone());
        serde_json::to_value(quote).map_err(failed)
    }
}

/// An order against a quote.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Order {
    /// Its id.
    pub id: String,
    /// The quote it keeps the price of.
    pub quote: QuoteDocument,
    /// Where it stands: `awaiting_payment`, `paid`, `return_approved`.
    pub status: String,
    /// When it was placed.
    pub placed_at: DateTime<Utc>,
}

/// An order waiting for its payment (spec 3.5 §7.16 `order`).
pub const AWAITING_PAYMENT: &str = "awaiting_payment";
/// An order whose payment a desk is taking: no second payment starts for it.
pub const PAYING: &str = "paying";
/// An order paid in full (spec 3.5 §7.5 "Commerce").
pub const PAID: &str = "paid";

/// The site's orders.
#[derive(Debug, Default)]
pub struct OrderBook {
    // A blocking lock, never held across an await: the edge reads a call's amount through
    // `CapabilityHandler::call_amount`, which is synchronous.
    orders: std::sync::Mutex<BTreeMap<String, Order>>,
}

impl OrderBook {
    /// The order `id`.
    pub async fn get(&self, id: &str) -> Option<Order> {
        self.order(id)
    }

    /// Moves order `id` to `status`, such as [`PAID`] once a payment desk took its money
    /// (spec 3.5 §7.5 "Commerce"); returns the order, or `None` when there is none.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_website::edge::handlers::{OrderBook, PAID};
    ///
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() {
    /// let orders = OrderBook::default();
    /// assert!(orders.set_status("order-1", PAID).await.is_none(), "no such order");
    /// # }
    /// ```
    pub async fn set_status(&self, id: &str, status: &str) -> Option<Order> {
        let mut orders = self.orders.lock().unwrap_or_else(PoisonError::into_inner);
        let order = orders.get_mut(id)?;
        order.status = status.to_string();
        Some(order.clone())
    }

    fn order(&self, id: &str) -> Option<Order> {
        self.orders.lock().unwrap_or_else(PoisonError::into_inner).get(id).cloned()
    }

    /// Moves order `id` from `from` to `to` in one step; `None` when there is no such order
    /// or it is not in `from`.
    fn transition(&self, id: &str, from: &str, to: &str) -> Option<Order> {
        let mut orders = self.orders.lock().unwrap_or_else(PoisonError::into_inner);
        let order = orders.get_mut(id).filter(|order| order.status == from)?;
        order.status = to.to_string();
        Some(order.clone())
    }

    /// Keeps `order` unless one with its id exists; returns the order kept.
    fn place(&self, order: Order) -> Order {
        self.orders
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(order.id.clone())
            .or_insert(order)
            .clone()
    }
}

/// Orders, their status and returns (spec 3.5 §7.16 `order`, A-I7, A-I25).
pub struct OrderHandler {
    quotes: Arc<QuoteBook>,
    orders: Arc<OrderBook>,
    desk: Option<Arc<dyn PaymentDesk>>,
}

impl OrderHandler {
    /// Orders against `quotes`, kept in `orders`.
    pub fn new(quotes: Arc<QuoteBook>, orders: Arc<OrderBook>) -> Self {
        Self { quotes, orders, desk: None }
    }

    /// Returns each new order with what `desk` needs to take its payment, such as an AP2
    /// desk's signed cart (spec 3.5 §7.5 "Commerce").
    #[must_use]
    pub fn with_payments(mut self, desk: Arc<dyn PaymentDesk>) -> Self {
        self.desk = Some(desk);
        self
    }
}

#[async_trait]
impl CapabilityHandler for OrderHandler {
    /// A `place_order` call's amount is its quote's total; a call naming an order, the
    /// order's total (spec 3.5 §7.4).
    fn call_amount(&self, input: &Value) -> Option<u64> {
        if let Some(quote) = input["quoteId"].as_str() {
            return self.quotes.quote(&quote.parse().ok()?).map(|quote| quote.total);
        }
        let order = input["orderId"].as_str()?;
        self.orders.order(order).map(|order| order.quote.total)
    }

    async fn handle(&self, call: CapabilityCall) -> Result<Value, CapabilityError> {
        let not_found = |what: &str| refused(ErrorCode::ResourceNotFound, format!("no {what}"));
        match call.capability.as_str() {
            "place_order" => {
                let id = call.input["quoteId"]
                    .as_str()
                    .and_then(|id| id.parse::<Uuid>().ok())
                    .ok_or_else(|| not_found("quote"))?;
                let quote = self.quotes.get(&id).await.ok_or_else(|| not_found("quote"))?;
                if quote.requester.is_some() && quote.requester != call.requester {
                    return Err(not_found("quote"));
                }
                if !quote.is_open(Utc::now()) {
                    return Err(refused(ErrorCode::QuoteExpired, "the quote has expired"));
                }
                // The idempotency key names the order, so a retry finds the same one (A-I7).
                let id = format!("order-{}", call.idempotency_key);
                let order = self.orders.order(&id).unwrap_or_else(|| Order {
                    id,
                    quote,
                    status: AWAITING_PAYMENT.into(),
                    placed_at: Utc::now(),
                });
                // The desk prepares the payment before the order is kept, so a failure keeps
                // nothing and the retry places it again.
                let payment = match &self.desk {
                    Some(desk) => desk.checkout(&order).await?,
                    None => None,
                };
                let order = self.orders.place(order);
                let mut placed = serde_json::to_value(order).map_err(failed)?;
                if let Some(payment) = payment {
                    placed["payment"] = payment;
                }
                Ok(placed)
            }
            "order_status" | "request_return" => {
                // An order is its buyer's: another requester learns nothing of it, as with
                // the quote it was placed against.
                let id = call.input["orderId"].as_str().unwrap_or_default();
                let order = self
                    .orders
                    .order(id)
                    .filter(|order| {
                        order.quote.requester.is_none() || order.quote.requester == call.requester
                    })
                    .ok_or_else(|| not_found("order"))?;
                if call.capability == "order_status" {
                    return Ok(
                        json!({ "orderId": order.id, "status": order.status, "total": order.quote.total, "currency": order.quote.currency }),
                    );
                }
                let order = self
                    .orders
                    .set_status(&order.id, "return_approved")
                    .await
                    .ok_or_else(|| not_found("order"))?;
                Ok(json!({ "orderId": order.id, "status": order.status }))
            }
            other => Err(failed(format!("{other} is not an order capability"))),
        }
    }
}

/// Takes payments for the site under the owner's accounts (spec 3.5 §7.5 "Commerce",
/// §7.14). A rail adapter implements it, such as [`Ap2Desk`](super::ap2::Ap2Desk); the site
/// holds no custody of its own.
#[async_trait]
pub trait PaymentDesk: Send + Sync {
    /// Takes the payment `call` asks for and returns its receipt.
    ///
    /// # Errors
    ///
    /// The rail's refusal or failure.
    async fn take(&self, call: &CapabilityCall) -> Result<Value, CapabilityError>;

    /// Takes the payment for `order` that `call` asks for, such as `pay_order` with the
    /// buyer's mandate or rail credential, and returns its receipt (spec 3.5 §7.5
    /// "Commerce"). The [`PaymentHandler`] finds the order the call names, refuses one that
    /// is not awaiting payment, and marks the order [`PAID`] when the answer's `status` is
    /// `paid`. The default answers as [`take`](Self::take).
    ///
    /// # Errors
    ///
    /// The rail's refusal or failure.
    async fn take_for(
        &self,
        call: &CapabilityCall,
        order: &Order,
    ) -> Result<Value, CapabilityError> {
        let _ = order;
        self.take(call).await
    }

    /// What the buyer needs to pay `order` through this desk, returned with the order as its
    /// `payment` member, such as an AP2 desk's signed cart mandate; `None` when nothing is
    /// needed. The default needs nothing.
    ///
    /// # Errors
    ///
    /// The rail's refusal or failure; the order is not placed.
    async fn checkout(&self, order: &Order) -> Result<Option<Value>, CapabilityError> {
        let _ = order;
        Ok(None)
    }
}

/// Payments, donations, subscriptions and tickets (spec 3.5 §7.16 `order`, §7.5).
pub struct PaymentHandler {
    desk: Option<Arc<dyn PaymentDesk>>,
    orders: Option<Arc<OrderBook>>,
}

impl PaymentHandler {
    /// Payments through `desk`, or refused without one.
    pub fn new(desk: Option<Arc<dyn PaymentDesk>>) -> Self {
        Self { desk, orders: None }
    }

    /// Pays the orders in `orders`: `pay_order` names an order awaiting payment, the desk
    /// takes the payment for it, and the order is paid once (spec 3.5 §7.5 "Commerce").
    #[must_use]
    pub fn with_orders(mut self, orders: Arc<OrderBook>) -> Self {
        self.orders = Some(orders);
        self
    }
}

#[async_trait]
impl CapabilityHandler for PaymentHandler {
    async fn handle(&self, call: CapabilityCall) -> Result<Value, CapabilityError> {
        let Some(desk) = &self.desk else {
            return Err(refused(
                ErrorCode::PaymentPolicyRejected,
                "the site takes no payments until its owner connects a payment rail",
            ));
        };
        let (Some(orders), Some(id)) = (&self.orders, call.input["orderId"].as_str()) else {
            return desk.take(&call).await;
        };
        let not_found = || refused(ErrorCode::ResourceNotFound, "no order");
        let order = orders.order(id).ok_or_else(not_found)?;
        if order.quote.requester.is_some() && order.quote.requester != call.requester {
            return Err(not_found());
        }
        // One payment at a time per order: the order is `paying` while the desk takes it, so a
        // second call, under another key, is refused instead of paying twice.
        let Some(paying) = orders.transition(&order.id, AWAITING_PAYMENT, PAYING) else {
            let status = orders.order(&order.id).map_or(order.status, |order| order.status);
            return Err(refused(
                ErrorCode::PaymentPolicyRejected,
                format!("order {} is {status}, not awaiting payment", order.id),
            ));
        };
        let mut payment = Paying { orders: orders.as_ref(), id: &paying.id, settled: false };
        let receipt = desk.take_for(&call, &paying).await?;
        payment.settled = receipt["status"] == PAID;
        Ok(receipt)
    }

    /// A call naming an order is for the order's total; any other, its `amount` member
    /// (spec 3.5 §7.4).
    fn call_amount(&self, input: &Value) -> Option<u64> {
        match (&self.orders, input["orderId"].as_str()) {
            (Some(orders), Some(id)) => orders.order(id).map(|order| order.quote.total),
            _ => input.get("amount").and_then(Value::as_u64),
        }
    }
}

/// An order's payment in progress: when it ends — answered, refused, failed or dropped — the
/// order is paid if the desk settled it, and awaits payment again otherwise.
struct Paying<'a> {
    orders: &'a OrderBook,
    id: &'a str,
    settled: bool,
}

impl Drop for Paying<'_> {
    fn drop(&mut self) {
        let next = if self.settled { PAID } else { AWAITING_PAYMENT };
        self.orders.transition(self.id, PAYING, next);
    }
}

/// A bookable slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Slot {
    /// Its id.
    pub id: String,
    /// When it starts.
    pub starts_at: DateTime<Utc>,
    /// How long it lasts, in minutes.
    pub minutes: u32,
    /// What booking it costs, in minor units of the site's currency; free when not set. An
    /// owner's amount threshold on `book` compares it (spec 3.5 §7.4, S9).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<u64>,
}

/// The slots a site books (spec 3.5 §7.16 `booking`): a calendar driver implements it.
#[async_trait]
pub trait Calendar: Send + Sync {
    /// The open slots starting in `[from, to)`.
    ///
    /// # Errors
    ///
    /// The calendar's error.
    async fn open(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> adk_core::Result<Vec<Slot>>;

    /// Takes slot `id` for booking `key`; `false` when it is no longer open. Taking it again
    /// for the same key answers `true`.
    ///
    /// # Errors
    ///
    /// The calendar's error.
    async fn take(&self, id: &str, key: &str) -> adk_core::Result<bool>;

    /// The price of booking slot `id` in minor units, for an owner's amount threshold
    /// (spec 3.5 §7.4); `None` when the slot is free, unknown or priced elsewhere. Read
    /// synchronously while the edge decides the call. The default prices nothing.
    fn price(&self, id: &str) -> Option<u64> {
        let _ = id;
        None
    }
}

/// A [`Calendar`] in memory.
#[derive(Debug, Default)]
pub struct InMemoryCalendar {
    // A blocking lock, never held across an await, so `price` reads it synchronously.
    slots: std::sync::Mutex<BTreeMap<String, (Slot, Option<String>)>>,
}

impl InMemoryCalendar {
    /// A calendar with `slots` open.
    pub fn new(slots: impl IntoIterator<Item = Slot>) -> Self {
        Self {
            slots: std::sync::Mutex::new(
                slots.into_iter().map(|slot| (slot.id.clone(), (slot, None))).collect(),
            ),
        }
    }
}

#[async_trait]
impl Calendar for InMemoryCalendar {
    async fn open(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> adk_core::Result<Vec<Slot>> {
        Ok(self
            .slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .filter(|(slot, taken)| {
                taken.is_none() && slot.starts_at >= from && slot.starts_at < to
            })
            .map(|(slot, _)| slot.clone())
            .collect())
    }

    async fn take(&self, id: &str, key: &str) -> adk_core::Result<bool> {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(match slots.get_mut(id) {
            Some((_, taken @ None)) => {
                *taken = Some(key.to_string());
                true
            }
            Some((_, Some(holder))) => holder == key,
            None => false,
        })
    }

    fn price(&self, id: &str) -> Option<u64> {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner).get(id)?.0.price
    }
}

/// Availability and bookings (spec 3.5 §7.16 `booking`).
pub struct BookingHandler {
    calendar: Arc<dyn Calendar>,
    forms: Arc<FormHandler>,
}

impl BookingHandler {
    /// Bookings in `calendar`; a booking's contact is captured by `forms`.
    pub fn new(calendar: Arc<dyn Calendar>, forms: Arc<FormHandler>) -> Self {
        Self { calendar, forms }
    }
}

#[async_trait]
impl CapabilityHandler for BookingHandler {
    /// A booking's amount is its slot's price (spec 3.5 §7.4).
    fn call_amount(&self, input: &Value) -> Option<u64> {
        self.calendar.price(input["slot"].as_str()?)
    }

    async fn handle(&self, call: CapabilityCall) -> Result<Value, CapabilityError> {
        let date = |key: &str| {
            call.input[key]
                .as_str()
                .and_then(|d| d.parse::<chrono::NaiveDate>().ok())
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .map(|d| d.and_utc())
        };
        if call.capability == "check_availability" {
            let from = date("from").unwrap_or_else(Utc::now);
            let to = date("to").unwrap_or(from + Duration::days(14));
            let open = self.calendar.open(from, to).await.map_err(failed)?;
            return Ok(json!({ "slots": open }));
        }
        let slot = call.input["slot"].as_str().unwrap_or_default().to_string();
        if !self.calendar.take(&slot, &call.idempotency_key).await.map_err(failed)? {
            return Err(refused(ErrorCode::InventoryUnavailable, "that slot is no longer open"));
        }
        let mut booked = self.forms.handle(call).await?;
        booked["slot"] = json!(slot);
        booked["booked"] = json!(true);
        Ok(booked)
    }
}

/// Search over the release's index (spec 3.5 §7.10 `search_site`).
pub struct SearchHandler {
    search: Arc<dyn SiteSearch>,
}

impl SearchHandler {
    /// Search through `search`.
    pub fn new(search: Arc<dyn SiteSearch>) -> Self {
        Self { search }
    }
}

#[async_trait]
impl CapabilityHandler for SearchHandler {
    async fn handle(&self, call: CapabilityCall) -> Result<Value, CapabilityError> {
        let query = call.input["query"].as_str().unwrap_or_default();
        let hits = self.search.search(query, 10).await.map_err(failed)?;
        Ok(
            json!({ "results": hits.iter().map(|h| json!({ "path": h.path, "title": h.title, "snippet": h.snippet })).collect::<Vec<_>>() }),
        )
    }
}

/// Published items as read-only components (spec 3.5 §7.16 `components`, A-I26).
pub struct ListingHandler {
    content: PublishedContent,
    kind: ContentKind,
}

impl ListingHandler {
    /// The published items of `kind`.
    pub fn new(content: PublishedContent, kind: ContentKind) -> Self {
        Self { content, kind }
    }
}

#[async_trait]
impl CapabilityHandler for ListingHandler {
    async fn handle(&self, call: CapabilityCall) -> Result<Value, CapabilityError> {
        let query = call.input["query"].as_str().map(str::to_ascii_lowercase);
        let items: Vec<Value> = self
            .content
            .items(self.kind)
            .await?
            .into_iter()
            .filter(|item| {
                query.as_ref().is_none_or(|q| {
                    item.fields.values().any(|v| v.as_str().is_some_and(|s| s.to_ascii_lowercase().contains(q)))
                })
            })
            .map(|item| {
                let mut entry = json!({ "id": item.id.0, "slug": item.slug, "language": item.language.as_str() });
                for (key, value) in &item.fields {
                    if !item.volatile.contains(key) {
                        entry[key] = value.clone();
                    }
                }
                entry
            })
            .collect();
        Ok(json!({ "items": items, "release": self.content.release_seq().await }))
    }
}

/// An application's status by its reference (spec 3.5 §7.16.1 Government "status lookups").
pub struct StatusHandler {
    leads: Arc<dyn LeadStore>,
}

impl StatusHandler {
    /// Statuses of the applications in `leads`.
    pub fn new(leads: Arc<dyn LeadStore>) -> Self {
        Self { leads }
    }
}

#[async_trait]
impl CapabilityHandler for StatusHandler {
    async fn handle(&self, call: CapabilityCall) -> Result<Value, CapabilityError> {
        let reference = call.input["reference"].as_str().unwrap_or_default();
        let lead =
            self.leads.get(&LeadRef(reference.to_string())).await.map_err(failed)?.ok_or_else(
                || refused(ErrorCode::ResourceNotFound, "no application has that reference"),
            )?;
        Ok(json!({ "reference": lead.id.0, "status": "received", "receivedAt": lead.captured_at }))
    }
}
