//! One spend ledger for model calls and payments.
//!
//! A [`SpendLedger`] holds budget for spend that is about to happen and records spend
//! that did happen, so a model call and a checkout draw on the same limits:
//!
//! 1. [`reserve`](SpendLedger::reserve) holds an amount against every [`SpendLimits`]
//!    entry that covers the key, or fails with [`SpendError::LimitExceeded`].
//! 2. [`commit`](SpendLedger::commit) records the actual amount and drops the hold.
//! 3. [`release`](SpendLedger::release) drops the hold without recording spend.
//!
//! A hold that is neither committed nor released stops counting once its time to live
//! passes, so a process that dies mid-call does not lock budget forever.
//!
//! Amounts are integer micro-USD (`1_000_000` is one US dollar). Period windows start at
//! UTC midnight ([`SpendPeriod::Day`]) and on the first of the month at UTC midnight
//! ([`SpendPeriod::Month`]).

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::{AdkError, ErrorCategory, ErrorComponent, ErrorDetails, Result};

/// How long a reservation holds budget when it is neither committed nor released.
pub const DEFAULT_RESERVATION_TTL: Duration = Duration::from_secs(15 * 60);

/// Error code carried by the [`AdkError`] a ledger returns when a reservation would
/// exceed a limit.
pub const SPEND_LIMIT_EXCEEDED_CODE: &str = "spend.limit_exceeded";

/// Calendar window over which spend is summed. Boundaries are UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpendPeriod {
    /// The UTC calendar day.
    Day,
    /// The UTC calendar month.
    Month,
    /// All recorded spend.
    Lifetime,
}

impl SpendPeriod {
    /// Returns the inclusive start of the window that contains `at`, or `None` for
    /// [`SpendPeriod::Lifetime`].
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::SpendPeriod;
    /// use chrono::{TimeZone, Utc};
    ///
    /// let at = Utc.with_ymd_and_hms(2026, 10, 10, 17, 30, 0).unwrap();
    /// assert_eq!(
    ///     SpendPeriod::Month.window_start(at),
    ///     Some(Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap())
    /// );
    /// assert_eq!(SpendPeriod::Lifetime.window_start(at), None);
    /// ```
    pub fn window_start(self, at: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let day = match self {
            Self::Day => at.date_naive(),
            Self::Month => NaiveDate::from_ymd_opt(at.year(), at.month(), 1)?,
            Self::Lifetime => return None,
        };
        Some(day.and_time(NaiveTime::MIN).and_utc())
    }
}

impl fmt::Display for SpendPeriod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Day => "day",
            Self::Month => "month",
            Self::Lifetime => "lifetime",
        })
    }
}

/// Names a scope of spend and the window it is summed over.
///
/// `org` is required. `agent` and `vendor` narrow the scope; `None` means "any". Passed
/// to [`SpendLedger::reserve`], the key attributes the spend, and its `None` fields
/// record the spend as unattributed on that dimension. Used as a limit, the key bounds
/// every entry it [`covers`](Self::covers) within its `period`.
///
/// # Example
///
/// ```rust
/// use adk_core::{SpendKey, SpendPeriod};
///
/// let gemini_daily = SpendKey::org("acme").with_vendor("gemini").per(SpendPeriod::Day);
/// let call = SpendKey::org("acme").with_agent("researcher").with_vendor("gemini");
/// assert!(gemini_daily.covers(&call));
/// assert!(!gemini_daily.covers(&SpendKey::org("acme").with_vendor("openai")));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpendKey {
    /// Organization, tenant, or application the spend belongs to.
    pub org: String,
    /// Agent that spent, or `None` for any agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Vendor paid (model provider or merchant), or `None` for any vendor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    /// Window the key sums over when used as a limit.
    pub period: SpendPeriod,
}

impl SpendKey {
    /// Creates a key for all of `org`'s spend over [`SpendPeriod::Lifetime`].
    pub fn org(org: impl Into<String>) -> Self {
        Self { org: org.into(), agent: None, vendor: None, period: SpendPeriod::Lifetime }
    }

    /// Narrows the key to one agent.
    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }

    /// Narrows the key to one vendor.
    pub fn with_vendor(mut self, vendor: impl Into<String>) -> Self {
        self.vendor = Some(vendor.into());
        self
    }

    /// Sets the window the key sums over.
    pub fn per(mut self, period: SpendPeriod) -> Self {
        self.period = period;
        self
    }

    /// Returns `true` when spend attributed to `entry` falls inside this key's scope.
    ///
    /// The organizations must be equal, and each dimension this key names must equal the
    /// entry's. The periods are not compared.
    pub fn covers(&self, entry: &SpendKey) -> bool {
        self.org == entry.org
            && self.agent.as_ref().is_none_or(|agent| entry.agent.as_ref() == Some(agent))
            && self.vendor.as_ref().is_none_or(|vendor| entry.vendor.as_ref() == Some(vendor))
    }
}

impl fmt::Display for SpendKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "org={}", self.org)?;
        if let Some(agent) = &self.agent {
            write!(f, " agent={agent}")?;
        }
        if let Some(vendor) = &self.vendor {
            write!(f, " vendor={vendor}")?;
        }
        write!(f, " period={}", self.period)
    }
}

/// One cap: spend covered by `key` within `key.period` must not exceed `max_micro_usd`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpendLimit {
    /// Scope and window of the cap.
    pub key: SpendKey,
    /// Largest amount, in micro-USD, the scope may consume in one window.
    pub max_micro_usd: u64,
}

/// The caps a ledger enforces on every reservation.
///
/// Every limit whose key [covers](SpendKey::covers) a reservation's key is checked, so
/// an organization cap, an agent cap, and a vendor cap apply together.
///
/// # Example
///
/// ```rust
/// use adk_core::{SpendKey, SpendLimits, SpendPeriod};
///
/// let limits = SpendLimits::new()
///     .limit(SpendKey::org("acme").per(SpendPeriod::Day), 50_000_000)
///     .limit(SpendKey::org("acme").with_vendor("gemini").per(SpendPeriod::Month), 500_000_000);
/// assert_eq!(limits.applicable(&SpendKey::org("acme").with_vendor("gemini")).count(), 2);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpendLimits {
    limits: Vec<SpendLimit>,
}

impl SpendLimits {
    /// Creates an empty set, which lets every reservation through.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a cap of `max_micro_usd` on spend covered by `key` within `key.period`.
    pub fn limit(mut self, key: SpendKey, max_micro_usd: u64) -> Self {
        self.limits.push(SpendLimit { key, max_micro_usd });
        self
    }

    /// Returns every configured cap.
    pub fn iter(&self) -> impl Iterator<Item = &SpendLimit> {
        self.limits.iter()
    }

    /// Returns the caps that apply to spend attributed to `entry`.
    pub fn applicable<'a>(&'a self, entry: &'a SpendKey) -> impl Iterator<Item = &'a SpendLimit> {
        self.limits.iter().filter(move |limit| limit.key.covers(entry))
    }
}

/// Identifies one reservation in a ledger.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReservationId(String);

impl ReservationId {
    /// Creates a random identifier.
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// Returns the identifier as a string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for ReservationId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl fmt::Display for ReservationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Failures specific to spend ledgers. Converts into [`AdkError`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpendError {
    /// The reservation would take a scope past its cap.
    #[error(
        "spend limit exceeded for {limit}: {consumed_micro_usd} micro-USD already consumed plus {requested_micro_usd} requested exceeds the cap of {max_micro_usd}. Raise the limit or wait for the window to reset"
    )]
    LimitExceeded {
        /// The cap that refused the reservation.
        limit: SpendKey,
        /// The cap in micro-USD.
        max_micro_usd: u64,
        /// Committed spend plus live holds in the cap's current window.
        consumed_micro_usd: u64,
        /// The amount the caller asked to reserve.
        requested_micro_usd: u64,
    },
    /// The reservation does not exist or was already committed or released.
    #[error(
        "reservation {0} is unknown or already settled. Commit or release each reservation exactly once"
    )]
    UnknownReservation(ReservationId),
    /// The amount does not fit the ledger's storage.
    #[error("amount of {0} micro-USD is larger than the ledger can store")]
    AmountTooLarge(u64),
    /// The ledger's backing store could not be reached or failed.
    #[error("spend ledger unavailable: {0}. No budget is reserved while the ledger is unreachable")]
    Unavailable(String),
}

impl From<SpendError> for AdkError {
    fn from(error: SpendError) -> Self {
        let message = error.to_string();
        match error {
            SpendError::LimitExceeded {
                limit,
                max_micro_usd,
                consumed_micro_usd,
                requested_micro_usd,
            } => {
                let mut details = ErrorDetails::default();
                details.metadata.insert(
                    "limit".to_string(),
                    serde_json::to_value(&limit).unwrap_or(serde_json::Value::Null),
                );
                details.metadata.insert("max_micro_usd".to_string(), max_micro_usd.into());
                details
                    .metadata
                    .insert("consumed_micro_usd".to_string(), consumed_micro_usd.into());
                details
                    .metadata
                    .insert("requested_micro_usd".to_string(), requested_micro_usd.into());
                AdkError::new(
                    ErrorComponent::Guardrail,
                    ErrorCategory::Forbidden,
                    SPEND_LIMIT_EXCEEDED_CODE,
                    message,
                )
                .with_details(details)
            }
            SpendError::UnknownReservation(_) => AdkError::new(
                ErrorComponent::Guardrail,
                ErrorCategory::NotFound,
                "spend.unknown_reservation",
                message,
            ),
            SpendError::AmountTooLarge(_) => AdkError::new(
                ErrorComponent::Guardrail,
                ErrorCategory::InvalidInput,
                "spend.amount_too_large",
                message,
            ),
            SpendError::Unavailable(_) => AdkError::new(
                ErrorComponent::Guardrail,
                ErrorCategory::Unavailable,
                "spend.ledger_unavailable",
                message,
            ),
        }
    }
}

/// Converts a USD amount, such as [`UsageMetadata::cost`](crate::UsageMetadata::cost), to
/// micro-USD, rounding up. Returns `None` for a negative, NaN, or infinite amount.
///
/// # Example
///
/// ```rust
/// use adk_core::usd_to_micro_usd;
///
/// assert_eq!(usd_to_micro_usd(0.0125), Some(12_500));
/// assert_eq!(usd_to_micro_usd(-1.0), None);
/// ```
pub fn usd_to_micro_usd(usd: f64) -> Option<u64> {
    if !usd.is_finite() || usd < 0.0 {
        return None;
    }
    // Rounded first so binary noise (0.0125 * 1e6 = 12500.000000000002) does not add a
    // micro-USD, then up so a fractional micro-USD is never dropped.
    let micro = (usd * 1_000_000.0 * 1_000.0).round() / 1_000.0;
    Some(micro.ceil() as u64)
}

/// Durable record of reserved and committed spend, shared by model calls and payments.
///
/// Implementations must make [`reserve`](Self::reserve) atomic with respect to other
/// reservations: two concurrent calls must never both succeed when together they exceed
/// a cap. A reservation that is neither committed nor released stops counting after the
/// ledger's time to live.
///
/// # Example
///
/// ```rust
/// use adk_core::{InMemorySpendLedger, SpendKey, SpendLedger, SpendLimits, SpendPeriod};
///
/// # async fn demo() -> adk_core::Result<()> {
/// let ledger = InMemorySpendLedger::new(
///     SpendLimits::new().limit(SpendKey::org("acme").per(SpendPeriod::Day), 50_000_000),
/// );
/// let key = SpendKey::org("acme").with_agent("researcher").with_vendor("gemini");
///
/// let hold = ledger.reserve(&key, 20_000).await?;
/// ledger.commit(hold, 12_500).await?;
/// assert_eq!(ledger.spent(&SpendKey::org("acme"), SpendPeriod::Day).await?, 12_500);
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait SpendLedger: fmt::Debug + Send + Sync {
    /// Holds `amount_micro_usd` for spend attributed to `key`.
    ///
    /// `key.period` is not consulted; every configured limit that covers `key` is
    /// checked against committed spend plus live holds in that limit's current window.
    ///
    /// # Errors
    ///
    /// Returns an [`AdkError`] with code [`SPEND_LIMIT_EXCEEDED_CODE`] when a limit would
    /// be exceeded, or `spend.ledger_unavailable` when the backing store fails. Neither
    /// leaves a hold behind.
    async fn reserve(&self, key: &SpendKey, amount_micro_usd: u64) -> Result<ReservationId>;

    /// Records `actual_micro_usd` as spent and drops the reservation's hold.
    ///
    /// The amount is recorded even when it exceeds the reservation, a limit, or the
    /// reservation has expired: the spend already happened, and later reservations must
    /// see it.
    ///
    /// # Errors
    ///
    /// Returns `spend.unknown_reservation` when `id` is unknown or already settled.
    async fn commit(&self, id: ReservationId, actual_micro_usd: u64) -> Result<()>;

    /// Drops the reservation's hold without recording spend.
    ///
    /// # Errors
    ///
    /// Returns `spend.unknown_reservation` when `id` is unknown or already settled.
    async fn release(&self, id: ReservationId) -> Result<()>;

    /// Returns committed spend covered by `key` within the current `period` window.
    ///
    /// `key.period` is not consulted. Live holds are not included.
    ///
    /// # Errors
    ///
    /// Returns `spend.ledger_unavailable` when the backing store fails.
    async fn spent(&self, key: &SpendKey, period: SpendPeriod) -> Result<u64>;
}

/// A held reservation in [`InMemorySpendLedger`].
#[derive(Debug, Clone)]
struct Hold {
    key: SpendKey,
    amount: u64,
    expires_at: DateTime<Utc>,
}

/// Attribution of committed spend, without the period.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Attribution {
    org: String,
    agent: Option<String>,
    vendor: Option<String>,
}

impl Attribution {
    fn of(key: &SpendKey) -> Self {
        Self { org: key.org.clone(), agent: key.agent.clone(), vendor: key.vendor.clone() }
    }

    fn as_key(&self) -> SpendKey {
        SpendKey {
            org: self.org.clone(),
            agent: self.agent.clone(),
            vendor: self.vendor.clone(),
            period: SpendPeriod::Lifetime,
        }
    }
}

#[derive(Debug, Default)]
struct LedgerState {
    /// Committed spend per attribution and UTC day.
    committed: HashMap<(Attribution, NaiveDate), u64>,
    holds: HashMap<ReservationId, Hold>,
}

impl LedgerState {
    fn committed(&self, scope: &SpendKey, period: SpendPeriod, now: DateTime<Utc>) -> u64 {
        let since = period.window_start(now).map(|start| start.date_naive());
        self.committed
            .iter()
            .filter(|((attribution, day), _)| {
                since.is_none_or(|since| *day >= since) && scope.covers(&attribution.as_key())
            })
            .fold(0u64, |total, (_, amount)| total.saturating_add(*amount))
    }

    fn held(&self, scope: &SpendKey, now: DateTime<Utc>) -> u64 {
        self.holds
            .values()
            .filter(|hold| hold.expires_at > now && scope.covers(&hold.key))
            .fold(0u64, |total, hold| total.saturating_add(hold.amount))
    }
}

/// Process-local [`SpendLedger`] for tests, development, and single-process deployments.
///
/// Committed spend is kept per attribution and UTC day, so memory grows with the number
/// of distinct keys and days rather than with the number of calls. Nothing survives a
/// restart; use a SQL ledger from `adk-session` for durable budgets.
///
/// # Example
///
/// ```rust
/// use adk_core::{InMemorySpendLedger, SpendKey, SpendLimits, SpendPeriod};
/// use std::time::Duration;
///
/// let ledger = InMemorySpendLedger::new(
///     SpendLimits::new().limit(SpendKey::org("acme").per(SpendPeriod::Day), 50_000_000),
/// )
/// .with_reservation_ttl(Duration::from_secs(60));
/// # let _ = ledger;
/// ```
#[derive(Debug)]
pub struct InMemorySpendLedger {
    limits: SpendLimits,
    ttl: Duration,
    state: Mutex<LedgerState>,
}

impl InMemorySpendLedger {
    /// Creates a ledger enforcing `limits`, with [`DEFAULT_RESERVATION_TTL`].
    pub fn new(limits: SpendLimits) -> Self {
        Self { limits, ttl: DEFAULT_RESERVATION_TTL, state: Mutex::new(LedgerState::default()) }
    }

    /// Sets how long an unsettled reservation holds budget.
    pub fn with_reservation_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Returns the limits this ledger enforces.
    pub fn limits(&self) -> &SpendLimits {
        &self.limits
    }
}

impl Default for InMemorySpendLedger {
    fn default() -> Self {
        Self::new(SpendLimits::new())
    }
}

#[async_trait]
impl SpendLedger for InMemorySpendLedger {
    async fn reserve(&self, key: &SpendKey, amount_micro_usd: u64) -> Result<ReservationId> {
        let mut state = self.state.lock().await;
        let now = Utc::now();
        for limit in self.limits.applicable(key) {
            let consumed = state
                .committed(&limit.key, limit.key.period, now)
                .saturating_add(state.held(&limit.key, now));
            if consumed.saturating_add(amount_micro_usd) > limit.max_micro_usd {
                return Err(SpendError::LimitExceeded {
                    limit: limit.key.clone(),
                    max_micro_usd: limit.max_micro_usd,
                    consumed_micro_usd: consumed,
                    requested_micro_usd: amount_micro_usd,
                }
                .into());
            }
        }
        let ttl = chrono::Duration::from_std(self.ttl).unwrap_or(chrono::Duration::MAX);
        let id = ReservationId::generate();
        state.holds.insert(
            id.clone(),
            Hold {
                key: key.clone(),
                amount: amount_micro_usd,
                expires_at: now.checked_add_signed(ttl).unwrap_or(DateTime::<Utc>::MAX_UTC),
            },
        );
        Ok(id)
    }

    async fn commit(&self, id: ReservationId, actual_micro_usd: u64) -> Result<()> {
        let mut state = self.state.lock().await;
        let hold = state.holds.remove(&id).ok_or(SpendError::UnknownReservation(id))?;
        let bucket = (Attribution::of(&hold.key), Utc::now().date_naive());
        let total = state.committed.entry(bucket).or_default();
        *total = total.saturating_add(actual_micro_usd);
        Ok(())
    }

    async fn release(&self, id: ReservationId) -> Result<()> {
        let mut state = self.state.lock().await;
        state.holds.remove(&id).map(|_| ()).ok_or_else(|| SpendError::UnknownReservation(id).into())
    }

    async fn spent(&self, key: &SpendKey, period: SpendPeriod) -> Result<u64> {
        Ok(self.state.lock().await.committed(key, period, Utc::now()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::sync::Arc;

    fn daily_cap(org: &str, max: u64) -> SpendLimits {
        SpendLimits::new().limit(SpendKey::org(org).per(SpendPeriod::Day), max)
    }

    #[test]
    fn windows_start_at_utc_boundaries() {
        let at = Utc.with_ymd_and_hms(2026, 2, 28, 23, 59, 59).unwrap();
        assert_eq!(
            SpendPeriod::Day.window_start(at),
            Some(Utc.with_ymd_and_hms(2026, 2, 28, 0, 0, 0).unwrap())
        );
        assert_eq!(
            SpendPeriod::Month.window_start(at),
            Some(Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap())
        );
        assert_eq!(SpendPeriod::Lifetime.window_start(at), None);
    }

    #[test]
    fn a_key_covers_only_entries_inside_its_scope() {
        let entry = SpendKey::org("acme").with_agent("a").with_vendor("gemini");
        assert!(SpendKey::org("acme").covers(&entry));
        assert!(SpendKey::org("acme").with_agent("a").covers(&entry));
        assert!(!SpendKey::org("acme").with_agent("b").covers(&entry));
        assert!(!SpendKey::org("other").covers(&entry));
        // An unattributed entry is not inside a vendor-scoped cap.
        assert!(!SpendKey::org("acme").with_vendor("gemini").covers(&SpendKey::org("acme")));
    }

    #[test]
    fn usd_rounds_up_to_whole_micro_usd() {
        assert_eq!(usd_to_micro_usd(0.0), Some(0));
        assert_eq!(usd_to_micro_usd(1.0), Some(1_000_000));
        assert_eq!(usd_to_micro_usd(0.000_000_1), Some(1));
        assert_eq!(usd_to_micro_usd(f64::NAN), None);
        assert_eq!(usd_to_micro_usd(f64::INFINITY), None);
    }

    #[tokio::test]
    async fn a_reservation_over_the_cap_is_refused_with_a_structured_error() {
        let ledger = InMemorySpendLedger::new(daily_cap("acme", 100));
        let key = SpendKey::org("acme").with_vendor("gemini");
        let held = ledger.reserve(&key, 60).await.unwrap();

        let error = ledger.reserve(&key, 41).await.unwrap_err();
        assert_eq!(error.code, SPEND_LIMIT_EXCEEDED_CODE);
        assert_eq!(error.category, ErrorCategory::Forbidden);
        assert_eq!(error.details.metadata["consumed_micro_usd"], 60);
        assert_eq!(error.details.metadata["requested_micro_usd"], 41);

        ledger.release(held).await.unwrap();
        ledger.reserve(&key, 100).await.expect("a released hold frees its budget");
    }

    #[tokio::test]
    async fn commit_records_the_actual_amount_and_frees_the_hold() {
        let ledger = InMemorySpendLedger::new(daily_cap("acme", 100));
        let key = SpendKey::org("acme").with_agent("a").with_vendor("gemini");
        let held = ledger.reserve(&key, 80).await.unwrap();
        ledger.commit(held.clone(), 30).await.unwrap();

        assert_eq!(ledger.spent(&SpendKey::org("acme"), SpendPeriod::Day).await.unwrap(), 30);
        assert_eq!(
            ledger
                .spent(&SpendKey::org("acme").with_vendor("openai"), SpendPeriod::Day)
                .await
                .unwrap(),
            0
        );
        ledger.reserve(&key, 70).await.expect("only the committed 30 counts");
        assert_eq!(ledger.commit(held, 1).await.unwrap_err().code, "spend.unknown_reservation");
    }

    #[tokio::test]
    async fn concurrent_reservations_never_exceed_a_daily_cap() {
        let ledger = Arc::new(InMemorySpendLedger::new(daily_cap("acme", 1_000)));
        let tasks: Vec<_> = (0..64)
            .map(|i| {
                let ledger = ledger.clone();
                tokio::spawn(async move {
                    let key = SpendKey::org("acme").with_agent(format!("agent-{}", i % 4));
                    ledger.reserve(&key, 100).await
                })
            })
            .collect();
        let mut granted = 0;
        for task in tasks {
            if task.await.unwrap().is_ok() {
                granted += 1;
            }
        }
        assert_eq!(granted, 10);
    }

    #[tokio::test]
    async fn an_expired_reservation_stops_holding_budget() {
        let ledger = InMemorySpendLedger::new(daily_cap("acme", 100))
            .with_reservation_ttl(Duration::from_millis(20));
        let key = SpendKey::org("acme");
        let abandoned = ledger.reserve(&key, 100).await.unwrap();
        assert!(ledger.reserve(&key, 1).await.is_err());

        tokio::time::sleep(Duration::from_millis(60)).await;
        let next = ledger.reserve(&key, 100).await.expect("the expired hold is released");
        ledger.release(next).await.unwrap();

        // A late commit still records the spend that happened.
        ledger.commit(abandoned, 40).await.unwrap();
        assert_eq!(ledger.spent(&key, SpendPeriod::Day).await.unwrap(), 40);
    }
}
