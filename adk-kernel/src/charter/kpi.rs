//! KPI measurements and their daily and weekly aggregates (spec 3.5 §7.11).

use async_trait::async_trait;
use awp_types::Measurement;
use chrono::{DateTime, Datelike as _, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{KernelError, Result};

/// One measurement of a KPI, as the tracker read it from the KPI's source (spec 3.5 §7.11).
///
/// A KPI has at most one sample per instant: [`KpiStore::record`] keeps the first.
///
/// # Example
///
/// ```rust
/// use adk_kernel::charter::KpiSample;
///
/// let sample = KpiSample::new(uuid::Uuid::nil(), "leads", chrono::Utc::now(), 4.0, "commerce");
/// assert_eq!(serde_json::to_value(&sample).unwrap()["kpi"], "leads");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct KpiSample {
    /// The site.
    pub site: Uuid,
    /// The KPI's id in the charter.
    pub kpi: String,
    /// When it was measured.
    pub at: DateTime<Utc>,
    /// The value.
    pub value: f64,
    /// The source it came from (the KPI's `SourceRef`).
    pub source: String,
}

impl KpiSample {
    /// A sample of `kpi` on `site` measured at `at` from `source`.
    pub fn new(
        site: Uuid,
        kpi: impl Into<String>,
        at: DateTime<Utc>,
        value: f64,
        source: impl Into<String>,
    ) -> Self {
        Self { site, kpi: kpi.into(), at, value, source: source.into() }
    }

    /// Refuses a sample a store must not keep: an empty KPI id or a value that is not a
    /// finite number.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] naming the fault.
    pub fn validate(&self) -> Result<()> {
        if self.kpi.trim().is_empty() {
            return Err(KernelError::Config { reason: "a KPI sample needs the KPI's id".into() });
        }
        if !self.value.is_finite() {
            return Err(KernelError::Config {
                reason: format!(
                    "KPI {} measured {} at {}; a sample is a finite number, so fix the source",
                    self.kpi, self.value, self.at
                ),
            });
        }
        Ok(())
    }
}

/// The length of an aggregate's period (spec 3.5 §7.11): a UTC day, or an ISO week from
/// Monday.
///
/// # Example
///
/// ```rust
/// use chrono::NaiveDate;
/// use adk_kernel::charter::KpiPeriod;
///
/// let thursday = NaiveDate::from_ymd_opt(2026, 10, 22).unwrap();
/// assert_eq!(KpiPeriod::Week.start(thursday), NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
/// assert_eq!(KpiPeriod::Day.start(thursday), thursday);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum KpiPeriod {
    /// A UTC day.
    Day,
    /// An ISO week, Monday to Sunday, in UTC.
    Week,
}

impl KpiPeriod {
    /// The first day of the period holding `day`.
    pub fn start(self, day: NaiveDate) -> NaiveDate {
        period_start(self, day)
    }

    /// The number of days in a period.
    pub fn days(self) -> i64 {
        match self {
            Self::Day => 1,
            Self::Week => 7,
        }
    }
}

/// The first day of the `period` holding `day` (spec 3.5 §7.11): `day` itself, or the Monday
/// of its ISO week.
pub fn period_start(period: KpiPeriod, day: NaiveDate) -> NaiveDate {
    match period {
        KpiPeriod::Day => day,
        KpiPeriod::Week => day - Duration::days(i64::from(day.weekday().num_days_from_monday())),
    }
}

/// A KPI's samples over one period (spec 3.5 §7.11). A period without samples has
/// `count` 0 and no `min`, `max` or `last`, so a series has no gaps.
///
/// # Example
///
/// ```rust
/// use chrono::{NaiveDate, TimeZone, Utc};
/// use adk_kernel::charter::{KpiPeriod, KpiSample, aggregate};
///
/// let site = uuid::Uuid::nil();
/// let at = |d, h| Utc.with_ymd_and_hms(2026, 10, d, h, 0, 0).unwrap();
/// let samples = [KpiSample::new(site, "leads", at(20, 9), 3.0, "commerce"), KpiSample::new(site, "leads", at(20, 18), 5.0, "commerce")];
/// let days = aggregate(&samples, KpiPeriod::Day, NaiveDate::from_ymd_opt(2026, 10, 19).unwrap(), NaiveDate::from_ymd_opt(2026, 10, 21).unwrap());
/// assert_eq!(days.iter().map(|d| d.count).collect::<Vec<_>>(), [0, 2, 0]);
/// assert_eq!(days[1].mean(), Some(4.0));
/// assert_eq!(days[1].last, Some(5.0));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct KpiAggregate {
    /// The KPI.
    pub kpi: String,
    /// The period's length.
    pub period: KpiPeriod,
    /// The period's first day.
    pub start: NaiveDate,
    /// How many samples it holds.
    pub count: u64,
    /// Their sum.
    pub sum: f64,
    /// The smallest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    /// The largest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// The latest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<f64>,
}

impl KpiAggregate {
    /// The mean of the period's samples, `None` for an empty period.
    pub fn mean(&self) -> Option<f64> {
        (self.count > 0).then(|| self.sum / self.count as f64)
    }
}

/// The aggregates of `samples` per `period` (spec 3.5 §7.11), one for every period from the
/// one holding `from` through the one holding `to_inclusive`, in order, empty periods
/// included; samples of other KPIs than the first sample's, and samples outside those
/// periods, are left out.
pub fn aggregate(
    samples: &[KpiSample],
    period: KpiPeriod,
    from: NaiveDate,
    to_inclusive: NaiveDate,
) -> Vec<KpiAggregate> {
    let kpi = samples.first().map(|sample| sample.kpi.clone()).unwrap_or_default();
    let (first, last) = (period_start(period, from), period_start(period, to_inclusive));
    if last < first {
        return Vec::new();
    }
    let len = ((last - first).num_days() / period.days() + 1) as usize;
    let mut buckets: Vec<KpiAggregate> = (0..len)
        .map(|index| KpiAggregate {
            kpi: kpi.clone(),
            period,
            start: first + Duration::days(index as i64 * period.days()),
            count: 0,
            sum: 0.0,
            min: None,
            max: None,
            last: None,
        })
        .collect();
    let mut ordered: Vec<&KpiSample> = samples.iter().filter(|sample| sample.kpi == kpi).collect();
    ordered.sort_by_key(|sample| sample.at);
    for sample in ordered {
        let start = period_start(period, sample.at.date_naive());
        if start < first || start > last {
            continue;
        }
        let bucket = &mut buckets[((start - first).num_days() / period.days()) as usize];
        bucket.count += 1;
        bucket.sum += sample.value;
        bucket.min = Some(bucket.min.map_or(sample.value, |min| min.min(sample.value)));
        bucket.max = Some(bucket.max.map_or(sample.value, |max| max.max(sample.value)));
        bucket.last = Some(sample.value);
    }
    buckets
}

/// What the tracker knows about one KPI between ticks (spec 3.5 §7.11, A-I14): when its
/// measure-only period began, the baseline it locked when the period ended, and when it
/// last read the source.
///
/// # Example
///
/// ```rust
/// use adk_kernel::charter::KpiState;
///
/// let state = KpiState::new(uuid::Uuid::nil(), "leads", chrono::Utc::now());
/// assert!(state.baseline.is_none());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct KpiState {
    /// The site.
    pub site: Uuid,
    /// The KPI.
    pub kpi: String,
    /// When its measure-only period began: the first tick that saw the KPI.
    pub measuring_since: DateTime<Utc>,
    /// The baseline the tracker locked when the measure-only period ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<Measurement>,
    /// When the tracker last read the KPI's source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_read: Option<DateTime<Utc>>,
}

impl KpiState {
    /// A KPI first seen at `since`, with no baseline and no reading.
    pub fn new(site: Uuid, kpi: impl Into<String>, since: DateTime<Utc>) -> Self {
        Self { site, kpi: kpi.into(), measuring_since: since, baseline: None, last_read: None }
    }
}

/// Whether a guardrail is breached, and since when (spec 3.5 §7.11, A-I15): a breach pauses
/// and demotes its jobs once, when it begins, and a later breach after a recovery does so
/// again.
///
/// # Example
///
/// ```rust
/// use adk_kernel::charter::GuardrailState;
///
/// let state = GuardrailState::new(uuid::Uuid::nil(), "unsubscribes", chrono::Utc::now());
/// assert!(state.breached_since.is_none());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct GuardrailState {
    /// The site.
    pub site: Uuid,
    /// The guardrail.
    pub guardrail: String,
    /// When the current breach began; `None` while the metric is within the threshold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub breached_since: Option<DateTime<Utc>>,
    /// The metric's value at the last check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_value: Option<f64>,
    /// When it was last checked.
    pub checked_at: DateTime<Utc>,
}

impl GuardrailState {
    /// A guardrail checked at `at` and found within its threshold.
    pub fn new(site: Uuid, guardrail: impl Into<String>, at: DateTime<Utc>) -> Self {
        Self {
            site,
            guardrail: guardrail.into(),
            breached_since: None,
            last_value: None,
            checked_at: at,
        }
    }
}

/// The KPI time series of every site and the tracker's state (spec 3.5 §7.11), on
/// in-memory, SQLite, PostgreSQL and Redis.
///
/// # Example
///
/// ```rust
/// use chrono::{NaiveDate, TimeZone, Utc};
/// use adk_kernel::charter::{InMemoryKpiStore, KpiPeriod, KpiSample, KpiStore};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let store = InMemoryKpiStore::new();
/// let site = uuid::Uuid::nil();
/// let at = Utc.with_ymd_and_hms(2026, 10, 20, 9, 0, 0).unwrap();
/// assert!(store.record(KpiSample::new(site, "leads", at, 3.0, "commerce")).await?);
/// assert!(!store.record(KpiSample::new(site, "leads", at, 9.0, "commerce")).await?, "one sample per instant");
/// let day = NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
/// let week = store.aggregates(site, "leads", KpiPeriod::Week, day, day).await?;
/// assert_eq!((week.len(), week[0].sum), (1, 3.0));
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait KpiStore: Send + Sync {
    /// Records `sample`; `false`, keeping the stored one, when the KPI already has a sample
    /// at that instant (to the microsecond).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] for a sample [`KpiSample::validate`] refuses, or
    /// [`KernelError::Store`] when the backend fails.
    async fn record(&self, sample: KpiSample) -> Result<bool>;

    /// The samples of `kpi` on `site` measured in `[from, to)`, oldest first.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn samples(
        &self,
        site: Uuid,
        kpi: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<KpiSample>>;

    /// The tracker's state of `kpi` on `site`, or `None`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn state(&self, site: Uuid, kpi: &str) -> Result<Option<KpiState>>;

    /// Writes the tracker's state of one KPI over the stored one.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn put_state(&self, state: KpiState) -> Result<()>;

    /// The state of `guardrail` on `site`, or `None`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn guardrail(&self, site: Uuid, guardrail: &str) -> Result<Option<GuardrailState>>;

    /// Writes the state of one guardrail over the stored one.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn put_guardrail(&self, state: GuardrailState) -> Result<()>;

    /// The aggregates of `kpi` per `period` from the period holding `from` through the one
    /// holding `to_inclusive`: one per period, in order, empty periods included
    /// ([`aggregate`]).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Store`] when the backend fails.
    async fn aggregates(
        &self,
        site: Uuid,
        kpi: &str,
        period: KpiPeriod,
        from: NaiveDate,
        to_inclusive: NaiveDate,
    ) -> Result<Vec<KpiAggregate>> {
        let first = period_start(period, from);
        let end = period_start(period, to_inclusive) + Duration::days(period.days());
        let midnight = |day: NaiveDate| day.and_time(chrono::NaiveTime::MIN).and_utc();
        let samples = self.samples(site, kpi, midnight(first), midnight(end)).await?;
        let mut series = aggregate(&samples, period, from, to_inclusive);
        for bucket in &mut series {
            bucket.kpi = kpi.to_string();
        }
        Ok(series)
    }
}
