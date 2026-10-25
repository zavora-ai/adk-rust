//! Anomaly detection and wallet alarms (spec 3.5 §7.14, §7.21).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use adk_core::Result;
use async_trait::async_trait;
use awp_types::Urgency;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use super::policy::{AnomalyRule, SpendCategory, category_name};

/// What a wallet alarm is about.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum WalletAlarmKind {
    /// A payment would have brought a category's spend today to the anomaly rule's
    /// multiple of its trailing daily average; the category is paused.
    Anomaly {
        /// The paused category.
        category: SpendCategory,
        /// What the category had spent today, in minor units.
        today: u64,
        /// The payment that tripped the rule, in minor units.
        requested: u64,
        /// The trailing daily average, in minor units.
        trailing_average: f64,
        /// The rule's multiple.
        multiple: f64,
    },
    /// Reconciliation found discrepancies between the wallet and its providers.
    ReconciliationMismatch {
        /// Start of the reconciled window.
        from: DateTime<Utc>,
        /// End of the reconciled window.
        to: DateTime<Utc>,
        /// How many discrepancies it found.
        discrepancies: usize,
    },
}

/// An alarm the wallet raises to the owner (spec 3.5 §7.14, §7.21).
///
/// # Example
///
/// ```
/// use adk_payments::wallet::{SpendCategory, WalletAlarm, WalletAlarmKind};
/// use awp_types::Urgency;
///
/// let alarm = WalletAlarm {
///     wallet: "site-1".into(),
///     kind: WalletAlarmKind::Anomaly {
///         category: SpendCategory::Models,
///         today: 2_000,
///         requested: 1_000,
///         trailing_average: 1_000.0,
///         multiple: 3.0,
///     },
///     raised_at: chrono::Utc::now(),
/// };
/// assert_eq!(alarm.urgency(), Urgency::P1);
/// assert!(alarm.summary().contains("models"));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WalletAlarm {
    /// The wallet that raised it.
    pub wallet: String,
    /// What it is about.
    pub kind: WalletAlarmKind,
    /// When it was raised.
    pub raised_at: DateTime<Utc>,
}

impl WalletAlarm {
    /// How urgently the owner hears of it: an anomaly is P1, a reconciliation mismatch P2.
    pub fn urgency(&self) -> Urgency {
        match self.kind {
            WalletAlarmKind::Anomaly { .. } => Urgency::P1,
            WalletAlarmKind::ReconciliationMismatch { .. } => Urgency::P2,
        }
    }

    /// One line for the owner's card.
    pub fn summary(&self) -> String {
        match &self.kind {
            WalletAlarmKind::Anomaly { category, today, requested, trailing_average, multiple } => {
                format!(
                    "{} spending paused: {requested} more on top of {today} today would reach \
                     {multiple}x the daily average of {trailing_average:.0}; resume it once you \
                     have checked",
                    category_name(*category)
                )
            }
            WalletAlarmKind::ReconciliationMismatch { from, to, discrepancies } => format!(
                "reconciliation from {} to {} found {discrepancies} discrepancies with the \
                 providers",
                from.date_naive(),
                to.date_naive()
            ),
        }
    }
}

/// Where wallet alarms go: the owner's channels through the kernel's alarm path
/// (spec 3.5 §7.21).
#[async_trait]
pub trait WalletAlarmHook: Send + Sync {
    /// Raises `alarm`.
    ///
    /// # Errors
    ///
    /// Returns the hook's error; the wallet still pauses the category and logs it.
    async fn raise(&self, alarm: &WalletAlarm) -> Result<()>;
}

/// A [`WalletAlarmHook`] that keeps alarms in memory, for tests.
///
/// # Example
///
/// ```
/// use adk_payments::wallet::InMemoryAlarmHook;
///
/// assert!(InMemoryAlarmHook::new().alarms().is_empty());
/// ```
#[derive(Debug, Clone, Default)]
pub struct InMemoryAlarmHook {
    alarms: Arc<Mutex<Vec<WalletAlarm>>>,
}

impl InMemoryAlarmHook {
    /// A hook with no alarms yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every alarm raised, oldest first.
    pub fn alarms(&self) -> Vec<WalletAlarm> {
        self.alarms.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }
}

#[async_trait]
impl WalletAlarmHook for InMemoryAlarmHook {
    async fn raise(&self, alarm: &WalletAlarm) -> Result<()> {
        self.alarms.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(alarm.clone());
        Ok(())
    }
}

/// What the anomaly rule found for one prospective payment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Anomaly {
    pub(crate) today: u64,
    pub(crate) trailing_average: f64,
}

/// Judges a payment of `requested` minor units against `rule`, given the category's
/// autonomous spend per UTC day (`daily`, today included) and `today`.
///
/// History starts at the earliest day in `daily`; the average is over the
/// `min(trailing_days, history)` full days before today, empty days counting as zero.
pub(crate) fn judge(
    rule: &AnomalyRule,
    daily: &BTreeMap<NaiveDate, u64>,
    today: NaiveDate,
    requested: u64,
) -> Option<Anomaly> {
    let earliest = *daily.keys().next()?;
    let history = (today - earliest).num_days();
    if history <= 0 || history < i64::from(rule.min_history_days) {
        return None;
    }
    let days = history.min(i64::from(rule.trailing_days));
    let since = today - chrono::Duration::days(days);
    let trailing: u64 = daily.range(since..today).map(|(_, amount)| *amount).sum();
    if trailing == 0 {
        return None;
    }
    let trailing_average = trailing as f64 / days as f64;
    let spent_today = daily.get(&today).copied().unwrap_or(0);
    let would_be = spent_today.saturating_add(requested);
    if would_be < rule.min_amount || (would_be as f64) < rule.multiple * trailing_average {
        return None;
    }
    Some(Anomaly { today: spent_today, trailing_average })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(n: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, n).unwrap()
    }

    fn history(days: std::ops::Range<u32>, amount: u64) -> BTreeMap<NaiveDate, u64> {
        days.map(|n| (day(n), amount)).collect()
    }

    #[test]
    fn three_times_the_trailing_average_is_an_anomaly() {
        let rule = AnomalyRule::default();
        let mut daily = history(1..15, 1_000);
        daily.insert(day(15), 1_900);
        assert_eq!(judge(&rule, &daily, day(15), 1_000), None, "2_900 is under 3_000");
        let anomaly = judge(&rule, &daily, day(15), 1_100).expect("3_000 reaches 3x");
        assert_eq!(anomaly, Anomaly { today: 1_900, trailing_average: 1_000.0 });
    }

    #[test]
    fn a_young_or_idle_category_never_alarms() {
        let rule = AnomalyRule::default();
        let daily = history(10..15, 1_000);
        assert_eq!(judge(&rule, &daily, day(15), 100_000), None, "five days of history");
        assert_eq!(judge(&rule, &BTreeMap::new(), day(15), 100_000), None, "no history");
        let mut idle = BTreeMap::new();
        idle.insert(day(1), 0);
        assert_eq!(judge(&rule, &idle, day(15), 100_000), None, "no spend to average");
    }

    #[test]
    fn days_without_spend_lower_the_average() {
        let rule = AnomalyRule { min_history_days: 1, ..AnomalyRule::default() };
        // One 1_400 day fourteen days ago: the average over 14 days is 100.
        let daily = history(1..2, 1_400);
        let anomaly = judge(&rule, &daily, day(15), 300).expect("300 is 3x of 100");
        assert_eq!(anomaly.trailing_average, 100.0);
    }

    #[test]
    fn a_minimum_amount_keeps_small_days_quiet() {
        let rule = AnomalyRule { min_amount: 10_000, ..AnomalyRule::default() };
        let daily = history(1..15, 10);
        assert_eq!(judge(&rule, &daily, day(15), 100), None);
    }
}
