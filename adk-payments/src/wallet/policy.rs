//! The wallet policy: category mandates, the anomaly rule and the reconciliation cadence
//! (spec 3.5 §7.14, D-10).

use std::collections::BTreeMap;
use std::fmt;

use adk_core::{Result, SpendKey, SpendLimits, SpendPeriod};
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};

use super::error::{WALLET_INVALID_POLICY_CODE, invalid};
use crate::domain::Money;

/// The window a category cap is summed over (spec 3.5 §7.14).
///
/// The spend ledger's own windows, so the ledger checks the cap: a UTC day, a UTC month
/// (the default) or the wallet's lifetime, for a prepaid budget.
///
/// # Example
///
/// ```
/// use adk_payments::wallet::{Period, WalletPolicy};
///
/// assert_eq!(WalletPolicy::new("USD").period, Period::Month);
/// ```
pub type Period = SpendPeriod;

pub use awp_types::{Cadence, SpendCategory};

/// The wire name of `category`, such as `models`.
///
/// # Example
///
/// ```
/// use adk_payments::wallet::{SpendCategory, category_name};
///
/// assert_eq!(category_name(SpendCategory::Advertising), "advertising");
/// ```
pub fn category_name(category: SpendCategory) -> String {
    match serde_json::to_value(category) {
        Ok(serde_json::Value::String(name)) => name,
        _ => format!("{category:?}").to_ascii_lowercase(),
    }
}

/// The last full window of `cadence` before `now`, as `[from, to)`: yesterday, last ISO
/// week, last calendar month or last quarter, in UTC.
///
/// # Example
///
/// ```
/// use adk_payments::wallet::{Cadence, previous_window};
/// use chrono::{TimeZone, Utc};
///
/// let now = Utc.with_ymd_and_hms(2026, 11, 3, 9, 0, 0).unwrap();
/// let (from, to) = previous_window(Cadence::Monthly, now);
/// assert_eq!(from, Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap());
/// assert_eq!(to, Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap());
/// ```
pub fn previous_window(cadence: Cadence, now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    let today = now.date_naive();
    let month_start = first_of_month(today.year(), today.month());
    let (from, to) = match cadence {
        Cadence::Daily => (today - Duration::days(1), today),
        Cadence::Weekly => {
            let monday = today - Duration::days(i64::from(today.weekday().num_days_from_monday()));
            (monday - Duration::days(7), monday)
        }
        Cadence::Quarterly => {
            let first = first_of_month(today.year(), (today.month() - 1) / 3 * 3 + 1);
            (shift_months(first, -3), first)
        }
        // Monthly, and any cadence a later protocol version adds.
        _ => (shift_months(month_start, -1), month_start),
    };
    (midnight(from), midnight(to))
}

fn first_of_month(year: i32, month: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, 1).unwrap_or(NaiveDate::MIN)
}

fn shift_months(first: NaiveDate, months: i32) -> NaiveDate {
    let index = first.year() * 12 + first.month0() as i32 + months;
    first_of_month(index.div_euclid(12), index.rem_euclid(12) as u32 + 1)
}

pub(crate) fn midnight(day: NaiveDate) -> DateTime<Utc> {
    day.and_time(NaiveTime::MIN).and_utc()
}

/// Who receives a payment.
///
/// `id` is the payee's identifier on the rail: a merchant name for AP2, the `payTo` address
/// of a use-based challenge, the provider account for provider billing. `domain`, when
/// known, is the payee's web domain.
///
/// # Example
///
/// ```
/// use adk_payments::wallet::Payee;
///
/// let registrar = Payee::new("registrar-1").with_name("Registrar").with_domain("registrar.example");
/// assert_eq!(registrar.domain.as_deref(), Some("registrar.example"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Payee {
    /// The payee's identifier on the rail.
    pub id: String,
    /// The payee's display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The payee's web domain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

impl Payee {
    /// A payee identified by `id`.
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into(), name: None, domain: None }
    }

    /// Sets the display name.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Sets the web domain.
    #[must_use]
    pub fn with_domain(mut self, domain: impl Into<String>) -> Self {
        self.domain = Some(domain.into());
        self
    }
}

/// Which payees a mandate allows (spec 3.5 §7.14).
///
/// A mandate with no rules allows no payee: the owner names every payee, or names
/// [`Any`](Self::Any) explicitly.
///
/// # Example
///
/// ```
/// use adk_payments::wallet::{Payee, PayeeRule};
///
/// let rule = PayeeRule::Domain("anthropic.com".into());
/// assert!(rule.allows(&Payee::new("acct-1").with_domain("api.anthropic.com")));
/// assert!(!rule.allows(&Payee::new("acct-1").with_domain("notanthropic.com")));
/// assert_eq!(serde_json::to_value(&rule).unwrap(), serde_json::json!({ "domain": "anthropic.com" }));
/// assert_eq!(serde_json::to_value(PayeeRule::Any).unwrap(), "any");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PayeeRule {
    /// Any payee.
    Any,
    /// The payee whose id is exactly this.
    Id(String),
    /// Payees on this domain or one of its subdomains.
    Domain(String),
}

impl PayeeRule {
    /// Whether the rule allows `payee`.
    pub fn allows(&self, payee: &Payee) -> bool {
        match self {
            Self::Any => true,
            Self::Id(id) => payee.id == *id,
            Self::Domain(domain) => payee.domain.as_deref().is_some_and(|candidate| {
                let candidate = candidate.trim_end_matches('.').to_ascii_lowercase();
                let domain = domain.trim_end_matches('.').to_ascii_lowercase();
                !domain.is_empty()
                    && (candidate == domain || candidate.ends_with(&format!(".{domain}")))
            }),
        }
    }
}

/// The rail a mandate pays through (spec 3.5 §7.14): the name of a
/// [`PaymentRail`](super::PaymentRail) the wallet holds.
///
/// # Example
///
/// ```
/// use adk_payments::wallet::RailRef;
///
/// assert_eq!(RailRef::ap2().as_str(), "ap2");
/// assert_eq!(serde_json::to_value(RailRef::use_based()).unwrap(), "use_based");
/// assert_eq!(RailRef::provider_billing("hostco").as_str(), "provider_billing:hostco");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RailRef(String);

impl RailRef {
    /// The name of the AP2 mandate rail for purchases.
    pub const AP2: &'static str = "ap2";
    /// The name of the use-based rail for metered APIs and model tokens.
    pub const USE_BASED: &'static str = "use_based";
    /// The prefix of a provider-billing rail's name, `provider_billing:{provider}`.
    pub const PROVIDER_BILLING: &'static str = "provider_billing";

    /// A rail named `name`.
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// The AP2 mandate rail.
    pub fn ap2() -> Self {
        Self::new(Self::AP2)
    }

    /// The use-based rail.
    pub fn use_based() -> Self {
        Self::new(Self::USE_BASED)
    }

    /// The rail billing the owner's account at `provider`.
    pub fn provider_billing(provider: &str) -> Self {
        Self(format!("{}:{provider}", Self::PROVIDER_BILLING))
    }

    /// The rail's name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RailRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What the owner lets the site spend in one category (spec 3.5 §7.14).
///
/// Amounts are minor units of the wallet's currency. A payment over `max_per_payment`, or
/// one that would take the category past `cap_per_period`, is held for the owner; a payee
/// no rule in `payees` allows, or a rail other than `rail`, is refused.
///
/// # Example
///
/// ```
/// use adk_payments::wallet::{Mandate, PayeeRule, RailRef};
///
/// let models = Mandate::new(30_000, 2_000, RailRef::use_based())
///     .with_payee(PayeeRule::Domain("models.example".into()));
/// assert_eq!(models.cap_per_period, 30_000);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mandate {
    /// The most the category may spend in one period, in minor units.
    pub cap_per_period: u64,
    /// The most one payment may be, in minor units.
    pub max_per_payment: u64,
    /// The payees the site may pay in this category.
    pub payees: Vec<PayeeRule>,
    /// The rail payments in this category go through.
    pub rail: RailRef,
}

impl Mandate {
    /// A mandate capping the category at `cap_per_period` and each payment at
    /// `max_per_payment`, through `rail`, with no payee allowed yet.
    pub fn new(cap_per_period: u64, max_per_payment: u64, rail: RailRef) -> Self {
        Self { cap_per_period, max_per_payment, payees: Vec::new(), rail }
    }

    /// Allows payees matching `rule`.
    #[must_use]
    pub fn with_payee(mut self, rule: PayeeRule) -> Self {
        self.payees.push(rule);
        self
    }

    /// Whether some rule allows `payee`.
    pub fn allows_payee(&self, payee: &Payee) -> bool {
        self.payees.iter().any(|rule| rule.allows(payee))
    }
}

/// When spend in a category is anomalous (spec 3.5 §7.14).
///
/// A payment that would bring a category's spend today to `multiple` times its trailing
/// daily average — over the last `trailing_days` full days, once the category has
/// `min_history_days` of history — pauses the category, raises a P1 alarm and is refused.
/// Days below `min_amount` (minor units) never alarm, so a category spending cents does not
/// page the owner. Owner-approved payments neither trigger nor count toward the average.
///
/// # Example
///
/// ```
/// use adk_payments::wallet::AnomalyRule;
///
/// let rule = AnomalyRule::default();
/// assert_eq!(rule.multiple, 3.0);
/// assert_eq!(rule.trailing_days, 14);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnomalyRule {
    /// The multiple of the trailing daily average that pauses a category.
    pub multiple: f64,
    /// The full days before today the average is taken over.
    pub trailing_days: u32,
    /// The days of history a category needs before it can alarm.
    pub min_history_days: u32,
    /// The least a day's spend must reach, in minor units, before it can alarm.
    #[serde(default)]
    pub min_amount: u64,
}

impl Default for AnomalyRule {
    fn default() -> Self {
        Self { multiple: 3.0, trailing_days: 14, min_history_days: 7, min_amount: 0 }
    }
}

/// The owner's wallet policy (spec 3.5 §7.14, D-10).
///
/// The site reads it and never changes it: [`Wallet`](super::Wallet) takes it at
/// construction and exposes it only by shared reference, so no API path raises a cap, adds
/// a payee or changes a rail (A-I16). A new policy is a new wallet the owner builds.
///
/// # Example
///
/// ```
/// use adk_payments::wallet::{Mandate, PayeeRule, RailRef, SpendCategory, WalletPolicy};
///
/// let policy = WalletPolicy::new("USD").with_mandate(
///     SpendCategory::Domains,
///     Mandate::new(5_000, 3_000, RailRef::ap2()).with_payee(PayeeRule::Id("Registrar".into())),
/// );
/// policy.validate()?;
/// let limits = policy.spend_limits("site-1")?;
/// assert_eq!(limits.iter().count(), 1);
/// # Ok::<(), adk_core::AdkError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WalletPolicy {
    /// ISO 4217 currency code; every amount is in its minor units.
    pub currency: String,
    /// The window caps are summed over. Monthly by default.
    pub period: Period,
    /// The mandate per category; a category without one cannot be spent in.
    pub categories: BTreeMap<SpendCategory, Mandate>,
    /// When spend is anomalous.
    pub anomaly: AnomalyRule,
    /// How often the wallet reconciles against its providers. Monthly by default.
    pub reconciliation: Cadence,
}

impl WalletPolicy {
    /// A policy in `currency` with no mandates: monthly caps, the default anomaly rule and
    /// monthly reconciliation.
    pub fn new(currency: impl Into<String>) -> Self {
        Self {
            currency: currency.into().to_ascii_uppercase(),
            period: SpendPeriod::Month,
            categories: BTreeMap::new(),
            anomaly: AnomalyRule::default(),
            reconciliation: Cadence::Monthly,
        }
    }

    /// Sets the mandate for `category`.
    #[must_use]
    pub fn with_mandate(mut self, category: SpendCategory, mandate: Mandate) -> Self {
        self.categories.insert(category, mandate);
        self
    }

    /// Sets the window caps are summed over.
    #[must_use]
    pub fn with_period(mut self, period: Period) -> Self {
        self.period = period;
        self
    }

    /// Sets the anomaly rule.
    #[must_use]
    pub fn with_anomaly(mut self, anomaly: AnomalyRule) -> Self {
        self.anomaly = anomaly;
        self
    }

    /// Sets the reconciliation cadence.
    #[must_use]
    pub fn with_reconciliation(mut self, cadence: Cadence) -> Self {
        self.reconciliation = cadence;
        self
    }

    /// The mandate for `category`, if the owner gave one.
    pub fn mandate(&self, category: SpendCategory) -> Option<&Mandate> {
        self.categories.get(&category)
    }

    /// The ISO 4217 minor-unit scale of the currency: 2 for USD, 0 for JPY.
    ///
    /// # Errors
    ///
    /// Returns `wallet.invalid_policy` for a code that is not an ISO 4217 currency with a
    /// minor unit.
    pub fn scale(&self) -> Result<u32> {
        Money::iso_minor_unit_scale(&self.currency).ok_or_else(|| {
            invalid(
                WALLET_INVALID_POLICY_CODE,
                format!(
                    "`{}` is not an ISO 4217 currency with a minor unit; use a code such as USD or KES",
                    self.currency
                ),
            )
        })
    }

    /// Checks the policy can be enforced.
    ///
    /// # Errors
    ///
    /// Returns `wallet.invalid_policy` naming the fault: an unknown currency, a mandate whose
    /// cap does not fit the spend ledger, an anomaly multiple that is not a finite number above
    /// 1, or a trailing window of zero days.
    pub fn validate(&self) -> Result<()> {
        self.scale()?;
        for (category, mandate) in &self.categories {
            self.to_ledger_units(mandate.cap_per_period).ok_or_else(|| {
                invalid(
                    WALLET_INVALID_POLICY_CODE,
                    format!(
                        "the {} cap of {} does not fit the spend ledger",
                        category_name(*category),
                        mandate.cap_per_period
                    ),
                )
            })?;
            if mandate.rail.as_str().trim().is_empty() {
                return Err(invalid(
                    WALLET_INVALID_POLICY_CODE,
                    format!("the {} mandate names no rail", category_name(*category)),
                ));
            }
        }
        let anomaly = &self.anomaly;
        if !anomaly.multiple.is_finite() || anomaly.multiple <= 1.0 {
            return Err(invalid(
                WALLET_INVALID_POLICY_CODE,
                format!(
                    "the anomaly multiple must be a number above 1, not {}; 3 pauses a category at three times its daily average",
                    anomaly.multiple
                ),
            ));
        }
        if anomaly.trailing_days == 0 {
            return Err(invalid(
                WALLET_INVALID_POLICY_CODE,
                "the anomaly rule's trailing window needs at least one day",
            ));
        }
        Ok(())
    }

    /// The spend ledger limits that cap each category (spec 3.5 §7.14): one
    /// [`SpendLimit`](adk_core::SpendLimit) per mandate on the category's scope in
    /// [`category_scope`], over [`period`](Self::period).
    ///
    /// Give these limits to the ledger the wallet reserves in, so the ledger refuses an
    /// over-cap reservation atomically across processes. The ledger counts millionths of
    /// the currency's major unit: micro-USD for a USD wallet.
    ///
    /// # Errors
    ///
    /// Returns `wallet.invalid_policy` when the policy does not [validate](Self::validate).
    pub fn spend_limits(&self, wallet: &str) -> Result<SpendLimits> {
        self.validate()?;
        let mut limits = SpendLimits::new();
        for (category, mandate) in &self.categories {
            let cap = self.to_ledger_units(mandate.cap_per_period).unwrap_or(u64::MAX);
            limits = limits
                .limit(SpendKey::org(category_scope(wallet, *category)).per(self.period), cap);
        }
        Ok(limits)
    }

    /// `minor` units of the currency in the ledger's millionths of a major unit, or `None`
    /// when it does not fit.
    pub(crate) fn to_ledger_units(&self, minor: u64) -> Option<u64> {
        let scale = self.scale().ok()?;
        if scale <= 6 {
            minor.checked_mul(10u64.checked_pow(6 - scale)?)
        } else {
            Some(minor.div_ceil(10u64.checked_pow(scale - 6)?))
        }
    }

    /// Ledger units (millionths of a major unit) as minor units, rounding up.
    pub(crate) fn minor_from_ledger(&self, units: u64) -> u64 {
        let scale = self.scale().unwrap_or(2);
        if scale <= 6 {
            10u64.checked_pow(6 - scale).map_or(units, |factor| units.div_ceil(factor))
        } else {
            10u64
                .checked_pow(scale - 6)
                .and_then(|factor| units.checked_mul(factor))
                .unwrap_or(u64::MAX)
        }
    }

    /// `minor` units as major units, for KPI values.
    pub(crate) fn to_major(&self, minor: u64) -> f64 {
        let scale = self.scale().unwrap_or(2);
        minor as f64 / 10f64.powi(scale as i32)
    }
}

/// The spend ledger scope of `wallet`'s spend in `category` on the site's own authority:
/// the organization its [`spend_limits`](WalletPolicy::spend_limits) cap.
///
/// # Example
///
/// ```
/// use adk_payments::wallet::{SpendCategory, category_scope};
///
/// assert_eq!(category_scope("site-1", SpendCategory::Models), "wallet:site-1/models");
/// ```
pub fn category_scope(wallet: &str, category: SpendCategory) -> String {
    format!("wallet:{wallet}/{}", category_name(category))
}

/// The spend ledger scope of `wallet`'s spend in `category` that an owner approved beyond
/// the mandate. No limit covers it, so an approved payment always records, and the wallet
/// counts it toward the category's spend.
///
/// # Example
///
/// ```
/// use adk_payments::wallet::{SpendCategory, owner_approved_scope};
///
/// assert_eq!(
///     owner_approved_scope("site-1", SpendCategory::Advertising),
///     "wallet:site-1/advertising/owner-approved"
/// );
/// ```
pub fn owner_approved_scope(wallet: &str, category: SpendCategory) -> String {
    format!("wallet:{wallet}/{}/owner-approved", category_name(category))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn previous_windows_are_whole_calendar_periods() {
        let now = Utc.with_ymd_and_hms(2026, 1, 15, 10, 0, 0).unwrap();
        let at = |y, m, d| Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap();
        assert_eq!(previous_window(Cadence::Daily, now), (at(2026, 1, 14), at(2026, 1, 15)));
        // 2026-01-15 is a Thursday; the week began on Monday 12 January.
        assert_eq!(previous_window(Cadence::Weekly, now), (at(2026, 1, 5), at(2026, 1, 12)));
        assert_eq!(previous_window(Cadence::Monthly, now), (at(2025, 12, 1), at(2026, 1, 1)));
        assert_eq!(previous_window(Cadence::Quarterly, now), (at(2025, 10, 1), at(2026, 1, 1)));
        let may = Utc.with_ymd_and_hms(2026, 5, 31, 23, 0, 0).unwrap();
        assert_eq!(previous_window(Cadence::Quarterly, may), (at(2026, 1, 1), at(2026, 4, 1)));
    }

    #[test]
    fn ledger_units_are_millionths_of_the_major_unit() {
        assert_eq!(WalletPolicy::new("USD").to_ledger_units(1_250), Some(12_500_000));
        assert_eq!(WalletPolicy::new("JPY").to_ledger_units(1_200), Some(1_200_000_000));
        assert_eq!(WalletPolicy::new("KWD").to_ledger_units(1), Some(1_000));
        assert_eq!(WalletPolicy::new("USD").to_ledger_units(u64::MAX), None);
        assert!(WalletPolicy::new("XXX").validate().is_err());
    }

    #[test]
    fn a_policy_with_a_bad_anomaly_rule_is_refused() {
        let policy = WalletPolicy::new("USD")
            .with_anomaly(AnomalyRule { multiple: 0.5, ..AnomalyRule::default() });
        assert_eq!(policy.validate().unwrap_err().code, WALLET_INVALID_POLICY_CODE);
        let policy = WalletPolicy::new("USD")
            .with_anomaly(AnomalyRule { multiple: f64::NAN, ..AnomalyRule::default() });
        assert!(policy.validate().is_err());
    }

    #[test]
    fn an_empty_payee_list_allows_no_one() {
        let mandate = Mandate::new(100, 10, RailRef::ap2());
        assert!(!mandate.allows_payee(&Payee::new("anyone")));
        let mandate = mandate.with_payee(PayeeRule::Any);
        assert!(mandate.allows_payee(&Payee::new("anyone")));
    }
}
