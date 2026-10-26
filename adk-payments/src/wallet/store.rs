//! The wallet's journal: payments, held payments and paused categories
//! (spec 3.5 §7.14, §9.3).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use adk_core::Result;
use adk_core::principal::SubjectRef;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;

use super::policy::{Payee, RailRef, SpendCategory};
use super::provider::Debit;
use super::rail::RailSettlement;

/// Where a payment stands.
///
/// # Example
///
/// ```
/// use adk_payments::wallet::PaymentState;
///
/// assert!(PaymentState::Settled.is_spent());
/// assert!(!PaymentState::Held.is_spent());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PaymentState {
    /// Waiting for the owner's decision.
    Held,
    /// Allowed and being paid.
    Executing,
    /// Paid.
    Settled,
    /// The owner refused it.
    Rejected,
    /// The rail or provider failed; nothing was paid unless `debit_outstanding` says so.
    Failed,
}

impl PaymentState {
    /// Whether the payment's money is spent.
    pub fn is_spent(self) -> bool {
        matches!(self, Self::Settled)
    }

    /// Whether the payment can change no further.
    pub fn is_final(self) -> bool {
        matches!(self, Self::Settled | Self::Rejected | Self::Failed)
    }
}

/// Why a payment was held for the owner (spec 3.5 §7.14).
///
/// # Example
///
/// ```
/// use adk_payments::wallet::HoldReason;
///
/// let reason = HoldReason::OverCategoryCap { cap: 10_000, spent: 9_000, requested: 2_000 };
/// assert!(reason.to_string().contains("cap"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
#[non_exhaustive]
pub enum HoldReason {
    /// The payment is larger than the mandate's per-payment limit.
    OverPaymentLimit {
        /// The mandate's `max_per_payment`.
        max_per_payment: u64,
        /// The payment's amount.
        requested: u64,
    },
    /// The payment would take the category past its cap for the period.
    OverCategoryCap {
        /// The mandate's `cap_per_period`.
        cap: u64,
        /// What the category has spent or is spending this period.
        spent: u64,
        /// The payment's amount.
        requested: u64,
    },
}

impl std::fmt::Display for HoldReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OverPaymentLimit { max_per_payment, requested } => write!(
                f,
                "{requested} is over the mandate's limit of {max_per_payment} per payment"
            ),
            Self::OverCategoryCap { cap, spent, requested } => write!(
                f,
                "{requested} on top of {spent} spent this period is over the category cap of {cap}"
            ),
        }
    }
}

/// Who decided a held payment, and how (spec 3.5 §7.14, A-I16).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerDecisionRecord {
    /// The person who decided, or the subject the run's approval was given for.
    pub decided_by: SubjectRef,
    /// Whether the payment was approved.
    pub approved: bool,
    /// Where the decision came from: `owner` for a decision through the wallet's API,
    /// `governed_path` for a confirmation the run's approval sources gave.
    pub source: String,
    /// What the decider said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// When.
    pub decided_at: DateTime<Utc>,
}

/// One payment in the wallet's journal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentRecord {
    /// The payment's id; providers and rails see it as the payment reference.
    pub id: String,
    /// The wallet it belongs to.
    pub wallet: String,
    /// The caller's idempotency key: one payment per key.
    pub key: String,
    /// Digest of what was asked, so a reused key with another payment is refused.
    pub request_digest: String,
    /// The category it is spent in.
    pub category: SpendCategory,
    /// Who receives it.
    pub payee: Payee,
    /// Amount in minor units.
    pub amount: u64,
    /// ISO 4217 currency code.
    pub currency: String,
    /// The rail it goes through.
    pub rail: RailRef,
    /// What it is for.
    pub description: String,
    /// The rail's data, kept so a held payment can be paid once approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rail_data: Option<Value>,
    /// Where it stands.
    pub state: PaymentState,
    /// Why it was held, when it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold: Option<HoldReason>,
    /// The agent that asked for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The owner's decision on a held payment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<OwnerDecisionRecord>,
    /// The provider's debit, for a wallet-funded rail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debit: Option<Debit>,
    /// Whether a debit is still outstanding on a failed payment, for reconciliation.
    #[serde(default)]
    pub debit_outstanding: bool,
    /// What the rail reported once it settled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settlement: Option<RailSettlement>,
    /// Why it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// When it was asked for.
    pub created_at: DateTime<Utc>,
    /// When it settled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<DateTime<Utc>>,
}

impl PaymentRecord {
    /// Whether an owner approved it beyond the mandate.
    pub fn owner_approved(&self) -> bool {
        self.decision.as_ref().is_some_and(|decision| decision.approved)
    }

    /// The proof a use-based payment carries for the metered request's retry.
    pub fn use_based_proof(&self) -> Option<super::PaymentProof> {
        let proof = self.settlement.as_ref()?.proof.clone()?;
        serde_json::from_value(proof).ok()
    }
}

/// Which payments to list.
///
/// # Example
///
/// ```
/// use adk_payments::wallet::{PaymentQuery, PaymentState, SpendCategory};
///
/// let held = PaymentQuery::new("site-1").state(PaymentState::Held).category(SpendCategory::Advertising);
/// # let _ = held;
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaymentQuery {
    /// The wallet.
    pub wallet: String,
    /// Only this category.
    pub category: Option<SpendCategory>,
    /// Only this state.
    pub state: Option<PaymentState>,
    /// Only payments settled, or for unsettled ones asked for, at or after this time.
    pub from: Option<DateTime<Utc>>,
    /// Only payments settled, or asked for, before this time.
    pub to: Option<DateTime<Utc>>,
}

impl PaymentQuery {
    /// Every payment of `wallet`.
    pub fn new(wallet: impl Into<String>) -> Self {
        Self { wallet: wallet.into(), category: None, state: None, from: None, to: None }
    }

    /// Only payments in `category`.
    #[must_use]
    pub fn category(mut self, category: SpendCategory) -> Self {
        self.category = Some(category);
        self
    }

    /// Only payments in `state`.
    #[must_use]
    pub fn state(mut self, state: PaymentState) -> Self {
        self.state = Some(state);
        self
    }

    /// Only payments in `[from, to)`, by settlement time, else by when they were asked for.
    #[must_use]
    pub fn between(mut self, from: DateTime<Utc>, to: DateTime<Utc>) -> Self {
        self.from = Some(from);
        self.to = Some(to);
        self
    }

    /// Whether `record` matches.
    pub fn matches(&self, record: &PaymentRecord) -> bool {
        let at = record.settled_at.unwrap_or(record.created_at);
        record.wallet == self.wallet
            && self.category.is_none_or(|category| record.category == category)
            && self.state.is_none_or(|state| record.state == state)
            && self.from.is_none_or(|from| at >= from)
            && self.to.is_none_or(|to| at < to)
    }
}

/// A category paused by an anomaly alarm until an owner resumes it (spec 3.5 §7.14).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryPause {
    /// The wallet.
    pub wallet: String,
    /// The paused category.
    pub category: SpendCategory,
    /// Why.
    pub reason: String,
    /// When.
    pub paused_at: DateTime<Utc>,
}

/// Where a wallet keeps its journal and pauses.
///
/// Every query is scoped by wallet id, so one store serves several sites without one
/// seeing another's payments (A-I22).
/// [`conformance::wallet_store_conformance`](super::conformance::wallet_store_conformance)
/// checks an implementation.
#[async_trait]
pub trait WalletStore: Send + Sync {
    /// Stores `record` unless its wallet already has a payment with its key; returns the
    /// stored record and whether it was inserted.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    async fn insert(&self, record: PaymentRecord) -> Result<(PaymentRecord, bool)>;

    /// Replaces the stored payment with `record`'s id.
    ///
    /// # Errors
    ///
    /// Returns `wallet.payment_not_found` when there is none, or the store's error.
    async fn update(&self, record: &PaymentRecord) -> Result<()>;

    /// Replaces the stored payment with `record`'s id only while it is in `expected` state,
    /// as one compare-and-set (spec 3.5 §7.14, A-I16): returns `true` when it replaced the
    /// payment and `false` when the stored payment has moved to another state.
    ///
    /// Wallets decide a held payment through it, so of two decisions racing on one payment,
    /// from one instance or several sharing the store, exactly one proceeds.
    ///
    /// The provided implementation reads the payment and then updates it, which is atomic
    /// only while one wallet instance uses the store; a store shared by several instances
    /// overrides it with one atomic step, as the built-in stores do.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_payments::wallet::{InMemoryWalletStore, PaymentState, WalletStore};
    ///
    /// # async fn decide(store: &InMemoryWalletStore, mut held: adk_payments::wallet::PaymentRecord) -> adk_core::Result<()> {
    /// held.state = PaymentState::Executing;
    /// if !store.update_if(&held, PaymentState::Held).await? {
    ///     // Another decision got there first.
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `wallet.payment_not_found` when there is none, or the store's error.
    async fn update_if(&self, record: &PaymentRecord, expected: PaymentState) -> Result<bool> {
        let stored = self.get(&record.wallet, &record.id).await?.ok_or_else(|| {
            super::error::not_found(
                super::error::WALLET_PAYMENT_NOT_FOUND_CODE,
                format!("wallet {} has no payment {}", record.wallet, record.id),
            )
        })?;
        if stored.state != expected {
            return Ok(false);
        }
        self.update(record).await?;
        Ok(true)
    }

    /// The payment `id` of `wallet`.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    async fn get(&self, wallet: &str, id: &str) -> Result<Option<PaymentRecord>>;

    /// The payment of `wallet` with idempotency key `key`.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    async fn by_key(&self, wallet: &str, key: &str) -> Result<Option<PaymentRecord>>;

    /// The payments `query` matches, oldest first.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    async fn list(&self, query: &PaymentQuery) -> Result<Vec<PaymentRecord>>;

    /// Pauses a category; pausing a paused category keeps the first pause.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    async fn pause(&self, pause: CategoryPause) -> Result<()>;

    /// Lifts the pause of `category`, returning it, or `None` when it was not paused.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    async fn resume(&self, wallet: &str, category: SpendCategory) -> Result<Option<CategoryPause>>;

    /// The paused categories of `wallet`.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    async fn pauses(&self, wallet: &str) -> Result<Vec<CategoryPause>>;
}

#[derive(Debug, Default)]
struct StoreState {
    payments: Vec<PaymentRecord>,
    by_id: HashMap<(String, String), usize>,
    by_key: HashMap<(String, String), usize>,
    pauses: BTreeMap<(String, SpendCategory), CategoryPause>,
}

/// A process-local [`WalletStore`], for tests and single-process deployments.
///
/// # Example
///
/// ```
/// use adk_payments::wallet::InMemoryWalletStore;
///
/// let store = InMemoryWalletStore::new();
/// # let _ = store;
/// ```
#[derive(Debug, Clone, Default)]
pub struct InMemoryWalletStore {
    state: Arc<RwLock<StoreState>>,
}

impl InMemoryWalletStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl WalletStore for InMemoryWalletStore {
    async fn insert(&self, record: PaymentRecord) -> Result<(PaymentRecord, bool)> {
        let mut state = self.state.write().await;
        let key = (record.wallet.clone(), record.key.clone());
        if let Some(index) = state.by_key.get(&key) {
            return Ok((state.payments[*index].clone(), false));
        }
        let index = state.payments.len();
        state.by_id.insert((record.wallet.clone(), record.id.clone()), index);
        state.by_key.insert(key, index);
        state.payments.push(record.clone());
        Ok((record, true))
    }

    async fn update(&self, record: &PaymentRecord) -> Result<()> {
        let mut state = self.state.write().await;
        let index =
            *state.by_id.get(&(record.wallet.clone(), record.id.clone())).ok_or_else(|| {
                super::error::not_found(
                    super::error::WALLET_PAYMENT_NOT_FOUND_CODE,
                    format!("wallet {} has no payment {}", record.wallet, record.id),
                )
            })?;
        state.payments[index] = record.clone();
        Ok(())
    }

    async fn update_if(&self, record: &PaymentRecord, expected: PaymentState) -> Result<bool> {
        let mut state = self.state.write().await;
        let index =
            *state.by_id.get(&(record.wallet.clone(), record.id.clone())).ok_or_else(|| {
                super::error::not_found(
                    super::error::WALLET_PAYMENT_NOT_FOUND_CODE,
                    format!("wallet {} has no payment {}", record.wallet, record.id),
                )
            })?;
        if state.payments[index].state != expected {
            return Ok(false);
        }
        state.payments[index] = record.clone();
        Ok(true)
    }

    async fn get(&self, wallet: &str, id: &str) -> Result<Option<PaymentRecord>> {
        let state = self.state.read().await;
        Ok(state
            .by_id
            .get(&(wallet.to_string(), id.to_string()))
            .map(|index| state.payments[*index].clone()))
    }

    async fn by_key(&self, wallet: &str, key: &str) -> Result<Option<PaymentRecord>> {
        let state = self.state.read().await;
        Ok(state
            .by_key
            .get(&(wallet.to_string(), key.to_string()))
            .map(|index| state.payments[*index].clone()))
    }

    async fn list(&self, query: &PaymentQuery) -> Result<Vec<PaymentRecord>> {
        let state = self.state.read().await;
        Ok(state.payments.iter().filter(|record| query.matches(record)).cloned().collect())
    }

    async fn pause(&self, pause: CategoryPause) -> Result<()> {
        let mut state = self.state.write().await;
        state.pauses.entry((pause.wallet.clone(), pause.category)).or_insert(pause);
        Ok(())
    }

    async fn resume(&self, wallet: &str, category: SpendCategory) -> Result<Option<CategoryPause>> {
        Ok(self.state.write().await.pauses.remove(&(wallet.to_string(), category)))
    }

    async fn pauses(&self, wallet: &str) -> Result<Vec<CategoryPause>> {
        let state = self.state.read().await;
        Ok(state
            .pauses
            .iter()
            .filter(|((owner, _), _)| owner == wallet)
            .map(|(_, pause)| pause.clone())
            .collect())
    }
}
