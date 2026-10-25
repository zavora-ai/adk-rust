//! Funding providers: where the owner's money sits (spec 3.5 §7.14).
//!
//! The owner funds the wallet with a payment provider (stored value) or a prepaid virtual
//! card from an issuing adapter; the site holds no custody of its own. Neither trait has a
//! method to open an account, add a payment method, raise a limit or top up: those are the
//! owner's acts with the provider, outside the site (A-I16).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use adk_core::{AdkError, Clock, ErrorCategory, ErrorComponent, Result, SystemClock};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::error::{WALLET_CURRENCY_MISMATCH_CODE, WALLET_INSUFFICIENT_FUNDS_CODE, conflict};
use super::policy::Payee;

/// How the owner funds the wallet (spec 3.5 §7.14).
///
/// # Example
///
/// ```
/// use adk_payments::wallet::Funding;
///
/// assert_eq!(serde_json::to_value(Funding::PrepaidCard).unwrap(), "prepaid_card");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Funding {
    /// Stored value held by a payment provider on the owner's account.
    StoredValue,
    /// A prepaid virtual card issued to the owner through an issuing adapter.
    PrepaidCard,
}

/// A request to move money out of the owner's funds for one payment.
///
/// `reference` is the wallet's payment id: a provider answers a repeated reference with
/// the debit it already made, so a retried payment is debited once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebitRequest {
    /// The wallet's payment id.
    pub reference: String,
    /// Who the money is for.
    pub payee: Payee,
    /// Amount in minor units of `currency`.
    pub amount: u64,
    /// ISO 4217 currency code.
    pub currency: String,
    /// What the payment is for.
    pub description: String,
}

/// Money a provider moved out of the owner's funds for one payment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Debit {
    /// The provider's id for the debit.
    pub id: String,
    /// The wallet's payment id it was made for.
    pub reference: String,
    /// Amount in minor units.
    pub amount: u64,
    /// ISO 4217 currency code.
    pub currency: String,
    /// When the provider made it.
    pub at: DateTime<Utc>,
    /// A token a rail presents to the payee for these funds, such as a virtual card's id;
    /// never a card number or a secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub funding_token: Option<String>,
}

/// What a statement line records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StatementKind {
    /// Money left the owner's funds.
    Debit,
    /// A debit was returned.
    Reversal,
    /// The owner added funds.
    Funding,
}

/// One line of a provider's statement, as reconciliation reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatementEntry {
    /// The provider's id for the line.
    pub id: String,
    /// The wallet payment id the line belongs to, when the provider recorded one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// What the line records.
    pub kind: StatementKind,
    /// Amount in minor units.
    pub amount: u64,
    /// ISO 4217 currency code.
    pub currency: String,
    /// The payee, when the provider knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payee: Option<String>,
    /// When it happened.
    pub at: DateTime<Utc>,
}

/// Where the owner's money sits (spec 3.5 §7.14).
///
/// The wallet debits it for every payment a rail settles with the wallet's funds, voids
/// the debit when the payment does not happen, and reconciles against its statement. It
/// has no method to open an account, add a payment method, raise a limit or add funds:
/// the owner does those with the provider.
///
/// [`conformance::wallet_provider_conformance`](super::conformance::wallet_provider_conformance)
/// checks an implementation.
///
/// # Example
///
/// ```
/// use adk_payments::wallet::{DebitRequest, InMemoryWalletProvider, Payee, WalletProvider};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_core::Result<()> {
/// let provider = InMemoryWalletProvider::stored_value("treasury", "USD", 10_000);
/// let debit = provider
///     .debit(&DebitRequest {
///         reference: "pay-1".into(),
///         payee: Payee::new("registrar"),
///         amount: 1_500,
///         currency: "USD".into(),
///         description: "domain renewal".into(),
///     })
///     .await?;
/// assert_eq!(debit.amount, 1_500);
/// assert_eq!(provider.balance().await?, 8_500);
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait WalletProvider: Send + Sync {
    /// The provider's name, as reports and audit records cite it.
    fn name(&self) -> &str;

    /// How the owner funds the wallet through this provider.
    fn funding(&self) -> Funding;

    /// The ISO 4217 currency the funds are held in.
    fn currency(&self) -> &str;

    /// The funds available, in minor units.
    ///
    /// # Errors
    ///
    /// Returns the provider's error when it cannot answer.
    async fn balance(&self) -> Result<u64>;

    /// Moves `request.amount` out of the owner's funds for one payment.
    ///
    /// A repeated `reference` with the same amount returns the debit already made.
    ///
    /// # Errors
    ///
    /// Returns `wallet.insufficient_funds` when the funds do not cover the amount,
    /// `wallet.currency_mismatch` for another currency, `wallet.key_reused` for a repeated
    /// reference with another amount, or the provider's error.
    async fn debit(&self, request: &DebitRequest) -> Result<Debit>;

    /// Returns a debit's money because its payment did not happen. Voiding a voided debit
    /// does nothing.
    ///
    /// # Errors
    ///
    /// Returns the provider's error; the wallet then records the payment as failed with the
    /// debit outstanding, for reconciliation.
    async fn void(&self, debit: &Debit) -> Result<()>;

    /// The provider's statement lines in `[from, to)`.
    ///
    /// # Errors
    ///
    /// Returns the provider's error when it cannot answer.
    async fn statement(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<StatementEntry>>;
}

/// The state of an in-memory book of funds, shared by [`InMemoryWalletProvider`] and
/// [`InMemoryCardIssuer`].
#[derive(Debug)]
struct Book {
    name: String,
    currency: String,
    balance: u64,
    debits: HashMap<String, (Debit, bool)>,
    entries: Vec<StatementEntry>,
    next: u64,
}

impl Book {
    fn new(name: &str, currency: &str, balance: u64) -> Self {
        Self {
            name: name.to_string(),
            currency: currency.to_ascii_uppercase(),
            balance,
            debits: HashMap::new(),
            entries: Vec::new(),
            next: 0,
        }
    }

    fn next_id(&mut self, prefix: &str) -> String {
        self.next += 1;
        format!("{}-{prefix}-{}", self.name, self.next)
    }

    fn debit(
        &mut self,
        request: &DebitRequest,
        at: DateTime<Utc>,
        funding_token: Option<String>,
    ) -> Result<Debit> {
        if !request.currency.eq_ignore_ascii_case(&self.currency) {
            return Err(conflict(
                WALLET_CURRENCY_MISMATCH_CODE,
                format!(
                    "{} holds {}, so it cannot pay {} {}",
                    self.name, self.currency, request.amount, request.currency
                ),
            ));
        }
        if let Some((existing, _)) = self.debits.get(&request.reference) {
            if existing.amount != request.amount {
                return Err(conflict(
                    super::error::WALLET_KEY_REUSED_CODE,
                    format!(
                        "payment {} was already debited for {}, not {}",
                        request.reference, existing.amount, request.amount
                    ),
                ));
            }
            return Ok(existing.clone());
        }
        if self.balance < request.amount {
            return Err(AdkError::new(
                ErrorComponent::Guardrail,
                ErrorCategory::ResourceExhausted,
                WALLET_INSUFFICIENT_FUNDS_CODE,
                format!(
                    "{} has {} {} available and cannot cover {}; the owner adds funds with the provider",
                    self.name, self.balance, self.currency, request.amount
                ),
            ));
        }
        self.balance -= request.amount;
        let debit = Debit {
            id: self.next_id("debit"),
            reference: request.reference.clone(),
            amount: request.amount,
            currency: self.currency.clone(),
            at,
            funding_token,
        };
        self.entries.push(StatementEntry {
            id: debit.id.clone(),
            reference: Some(request.reference.clone()),
            kind: StatementKind::Debit,
            amount: request.amount,
            currency: self.currency.clone(),
            payee: Some(request.payee.id.clone()),
            at,
        });
        self.debits.insert(request.reference.clone(), (debit.clone(), false));
        Ok(debit)
    }

    fn void(&mut self, debit: &Debit, at: DateTime<Utc>) {
        let Some((recorded, voided)) = self.debits.get_mut(&debit.reference) else {
            return;
        };
        if *voided {
            return;
        }
        *voided = true;
        let amount = recorded.amount;
        self.balance = self.balance.saturating_add(amount);
        let id = self.next_id("reversal");
        self.entries.push(StatementEntry {
            id,
            reference: Some(debit.reference.clone()),
            kind: StatementKind::Reversal,
            amount,
            currency: self.currency.clone(),
            payee: None,
            at,
        });
    }

    fn fund(&mut self, amount: u64, at: DateTime<Utc>) {
        self.balance = self.balance.saturating_add(amount);
        let id = self.next_id("funding");
        self.entries.push(StatementEntry {
            id,
            reference: None,
            kind: StatementKind::Funding,
            amount,
            currency: self.currency.clone(),
            payee: None,
            at,
        });
    }

    fn statement(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<StatementEntry> {
        self.entries.iter().filter(|entry| entry.at >= from && entry.at < to).cloned().collect()
    }
}

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A stored-value [`WalletProvider`] kept in memory, so tests are hermetic.
///
/// [`fund`](Self::fund) stands for the owner adding money with the provider; it is not
/// part of [`WalletProvider`], so nothing the site holds can call it.
///
/// # Example
///
/// ```
/// use adk_payments::wallet::{Funding, InMemoryWalletProvider, WalletProvider};
///
/// let provider = InMemoryWalletProvider::stored_value("treasury", "USD", 0);
/// provider.fund(5_000);
/// assert_eq!(provider.funding(), Funding::StoredValue);
/// ```
#[derive(Debug, Clone)]
pub struct InMemoryWalletProvider {
    name: String,
    currency: String,
    book: Arc<Mutex<Book>>,
    clock: Arc<dyn Clock>,
}

impl InMemoryWalletProvider {
    /// A stored-value provider `name` holding `balance` minor units of `currency`.
    pub fn stored_value(name: &str, currency: &str, balance: u64) -> Self {
        Self {
            name: name.to_string(),
            currency: currency.to_ascii_uppercase(),
            book: Arc::new(Mutex::new(Book::new(name, currency, balance))),
            clock: Arc::new(SystemClock),
        }
    }

    /// Sets the clock that stamps debits and statement lines.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Adds `amount` to the funds: the owner topping up with the provider.
    pub fn fund(&self, amount: u64) {
        let at = self.clock.now();
        locked(&self.book).fund(amount, at);
    }

    /// Adds a statement line the wallet did not make, such as a charge the owner made with
    /// the provider directly; reconciliation reports it.
    pub fn push_statement_entry(&self, entry: StatementEntry) {
        locked(&self.book).entries.push(entry);
    }

    /// The funds available now, for tests.
    pub fn available(&self) -> u64 {
        locked(&self.book).balance
    }

    /// Every statement line so far, for tests.
    pub fn entries(&self) -> Vec<StatementEntry> {
        locked(&self.book).entries.clone()
    }
}

#[async_trait]
impl WalletProvider for InMemoryWalletProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn funding(&self) -> Funding {
        Funding::StoredValue
    }

    fn currency(&self) -> &str {
        &self.currency
    }

    async fn balance(&self) -> Result<u64> {
        Ok(locked(&self.book).balance)
    }

    async fn debit(&self, request: &DebitRequest) -> Result<Debit> {
        let at = self.clock.now();
        locked(&self.book).debit(request, at, None)
    }

    async fn void(&self, debit: &Debit) -> Result<()> {
        let at = self.clock.now();
        locked(&self.book).void(debit, at);
        Ok(())
    }

    async fn statement(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<StatementEntry>> {
        Ok(locked(&self.book).statement(from, to))
    }
}

/// A prepaid virtual card as its issuer reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepaidCard {
    /// The issuer's id for the card: a token, never the card number.
    pub card_id: String,
    /// The card number's last four digits, for display.
    pub last4: String,
    /// ISO 4217 currency code.
    pub currency: String,
    /// What the card can still spend, in minor units.
    pub remaining: u64,
    /// Whether the owner froze the card.
    pub frozen: bool,
}

/// An issuing platform holding the owner's prepaid virtual card (spec 3.5 §7.14).
///
/// The owner issues and loads the card with the platform; the adapter only reads it,
/// charges it and reverses charges. [`PrepaidCardProvider`] turns it into a
/// [`WalletProvider`].
#[async_trait]
pub trait CardIssuer: Send + Sync {
    /// The issuing platform's name.
    fn name(&self) -> &str;

    /// The card.
    ///
    /// # Errors
    ///
    /// Returns the platform's error when it cannot answer.
    async fn card(&self) -> Result<PrepaidCard>;

    /// Charges the card for one payment; a repeated `reference` returns the charge made.
    ///
    /// # Errors
    ///
    /// Returns `wallet.insufficient_funds` when the card cannot cover the amount, or the
    /// platform's error.
    async fn charge(&self, request: &DebitRequest) -> Result<Debit>;

    /// Reverses a charge whose payment did not happen.
    ///
    /// # Errors
    ///
    /// Returns the platform's error.
    async fn reverse(&self, debit: &Debit) -> Result<()>;

    /// The card's transactions in `[from, to)`.
    ///
    /// # Errors
    ///
    /// Returns the platform's error.
    async fn transactions(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<StatementEntry>>;
}

/// A [`WalletProvider`] over a prepaid virtual card from a [`CardIssuer`]
/// (spec 3.5 §7.14).
///
/// A frozen card pays nothing. Each debit carries the card's id as its funding token, so a
/// rail can present the card without the agent ever seeing its number.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use adk_payments::wallet::{Funding, InMemoryCardIssuer, PrepaidCardProvider, WalletProvider};
///
/// let issuer = Arc::new(InMemoryCardIssuer::new("issuer", "USD", 20_000));
/// let provider = PrepaidCardProvider::new(issuer, "USD");
/// assert_eq!(provider.funding(), Funding::PrepaidCard);
/// ```
pub struct PrepaidCardProvider {
    issuer: Arc<dyn CardIssuer>,
    currency: String,
}

impl PrepaidCardProvider {
    /// A provider over `issuer`'s card, which is loaded in `currency`. A debit checks the
    /// card's own currency too.
    pub fn new(issuer: Arc<dyn CardIssuer>, currency: &str) -> Self {
        Self { issuer, currency: currency.to_ascii_uppercase() }
    }
}

impl std::fmt::Debug for PrepaidCardProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrepaidCardProvider").field("issuer", &self.issuer.name()).finish()
    }
}

#[async_trait]
impl WalletProvider for PrepaidCardProvider {
    fn name(&self) -> &str {
        self.issuer.name()
    }

    fn funding(&self) -> Funding {
        Funding::PrepaidCard
    }

    fn currency(&self) -> &str {
        &self.currency
    }

    async fn balance(&self) -> Result<u64> {
        let card = self.issuer.card().await?;
        Ok(if card.frozen { 0 } else { card.remaining })
    }

    async fn debit(&self, request: &DebitRequest) -> Result<Debit> {
        let card = self.issuer.card().await?;
        if card.frozen {
            return Err(AdkError::new(
                ErrorComponent::Guardrail,
                ErrorCategory::Forbidden,
                WALLET_INSUFFICIENT_FUNDS_CODE,
                format!("card ending {} is frozen by its owner", card.last4),
            ));
        }
        if !card.currency.eq_ignore_ascii_case(&request.currency) {
            return Err(conflict(
                WALLET_CURRENCY_MISMATCH_CODE,
                format!(
                    "card ending {} is in {}, not {}",
                    card.last4, card.currency, request.currency
                ),
            ));
        }
        let mut debit = self.issuer.charge(request).await?;
        debit.funding_token.get_or_insert(card.card_id);
        Ok(debit)
    }

    async fn void(&self, debit: &Debit) -> Result<()> {
        self.issuer.reverse(debit).await
    }

    async fn statement(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<StatementEntry>> {
        self.issuer.transactions(from, to).await
    }
}

/// A [`CardIssuer`] kept in memory, so tests are hermetic.
///
/// [`load`](Self::load) and [`freeze`](Self::freeze) stand for the owner's acts with the
/// issuing platform; neither is part of [`CardIssuer`].
#[derive(Debug, Clone)]
pub struct InMemoryCardIssuer {
    name: String,
    book: Arc<Mutex<Book>>,
    card_id: String,
    frozen: Arc<Mutex<bool>>,
    clock: Arc<dyn Clock>,
}

impl InMemoryCardIssuer {
    /// An issuer `name` holding one card loaded with `remaining` minor units of `currency`.
    pub fn new(name: &str, currency: &str, remaining: u64) -> Self {
        Self {
            name: name.to_string(),
            book: Arc::new(Mutex::new(Book::new(name, currency, remaining))),
            card_id: format!("{name}-card-1"),
            frozen: Arc::new(Mutex::new(false)),
            clock: Arc::new(SystemClock),
        }
    }

    /// Sets the clock that stamps charges.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Loads `amount` onto the card: the owner's act with the issuing platform.
    pub fn load(&self, amount: u64) {
        let at = self.clock.now();
        locked(&self.book).fund(amount, at);
    }

    /// Freezes or unfreezes the card: the owner's act with the issuing platform.
    pub fn freeze(&self, frozen: bool) {
        *locked(&self.frozen) = frozen;
    }
}

#[async_trait]
impl CardIssuer for InMemoryCardIssuer {
    fn name(&self) -> &str {
        &self.name
    }

    async fn card(&self) -> Result<PrepaidCard> {
        let book = locked(&self.book);
        Ok(PrepaidCard {
            card_id: self.card_id.clone(),
            last4: "4242".to_string(),
            currency: book.currency.clone(),
            remaining: book.balance,
            frozen: *locked(&self.frozen),
        })
    }

    async fn charge(&self, request: &DebitRequest) -> Result<Debit> {
        let at = self.clock.now();
        locked(&self.book).debit(request, at, Some(self.card_id.clone()))
    }

    async fn reverse(&self, debit: &Debit) -> Result<()> {
        let at = self.clock.now();
        locked(&self.book).void(debit, at);
        Ok(())
    }

    async fn transactions(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<StatementEntry>> {
        Ok(locked(&self.book).statement(from, to))
    }
}
