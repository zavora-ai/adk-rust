//! Ledger statistics: per-dimension baselines and drift detection over a content-free
//! activity ledger (spec 3.2 §7.10, §10 E1).
//!
//! The agent kernel keeps an append-only, content-free activity ledger. This module holds
//! the statistics over it as pure functions — no I/O and no model — so the kernel's
//! maintenance tick and the `cargo adk eval --ledger` CI gate run the same code.
//!
//! | Rule | Value |
//! |------|-------|
//! | Baseline | Median and MAD over a rolling 28-day window, per dimension and [`DayClass`] |
//! | Warm-up | 14 days with a value in the baseline window before a dimension can deviate |
//! | Comparison windows | The last 7 and the last 14 days; a day class deviates only when both do |
//! | Deviation | `\|recent − baseline\| > max(k·MAD, floor)`, k = 2.5, a floor per dimension |
//! | Meaningful | Two dimensions deviating, or one deviating for 10 days running |
//! | Cooldown | 7 days per dimension after an observation includes it |
//!
//! The baseline window ends the day before the 14-day comparison window starts, so a drift
//! inside the comparison windows does not move the baseline it is measured against.
//!
//! The statistics take plain daily series — [`DailyValue`]s keyed by dimension name — so a
//! caller that aggregates its own ledger store passes series directly. [`LedgerRow`] is the
//! export format, one camelCase JSON object per line with the §7.10 columns, and
//! [`daily_series`] derives the dimensions an export carries.
//!
//! # Example
//!
//! ```
//! use std::collections::BTreeMap;
//!
//! use adk_eval::ledger::{DailyValue, DeviationConfig, assess};
//! use chrono::{Days, NaiveDate};
//!
//! // Six weeks: four with the last activity at 17:30 and 20 tool calls a day, then two
//! // with the last activity at 19:30 and 40 tool calls.
//! let start = NaiveDate::from_ymd_opt(2026, 8, 31).unwrap();
//! let daily = |before: f64, after: f64| -> Vec<DailyValue> {
//!     (0..42)
//!         .map(|day| DailyValue::new(start + Days::new(day), if day < 28 { before } else { after }))
//!         .collect()
//! };
//! let series = BTreeMap::from([
//!     ("activity.last_minute".to_string(), daily(1050.0, 1170.0)),
//!     ("tool_call.count".to_string(), daily(20.0, 40.0)),
//! ]);
//!
//! let assessment = assess(&series, &DeviationConfig::default()).unwrap();
//! assert_eq!(assessment.report.deviating(), ["activity.last_minute", "tool_call.count"]);
//! // Two deviating dimensions are a meaningful drift.
//! assert!(assessment.meaningful);
//! ```

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Datelike, Days, FixedOffset, NaiveDate, Timelike, Weekday};
use serde::{Deserialize, Serialize};

/// Days in the rolling baseline window (spec 3.2 §7.10).
pub const BASELINE_WINDOW_DAYS: u32 = 28;

/// Days with a value in the baseline window before a dimension can deviate (spec 3.2 §7.10).
pub const WARM_UP_DAYS: u32 = 14;

/// The comparison windows, in days, each ending on the day under test (spec 3.2 §7.10).
pub const RECENT_WINDOW_DAYS: [u32; 2] = [7, 14];

/// The MAD multiplier of the deviation test (spec 3.2 §7.10).
pub const DEVIATION_K: f64 = 2.5;

/// Deviating dimensions that make a drift meaningful on their own (spec 3.2 §7.10).
pub const MEANINGFUL_DIMENSIONS: usize = 2;

/// Days a single dimension deviates in a row before its drift is meaningful (spec 3.2 §7.10).
pub const MEANINGFUL_RUN_DAYS: u32 = 10;

/// Days after an observation before it can include the same dimension again (spec 3.2 §7.10).
pub const COOLDOWN_DAYS: u32 = 7;

/// The floor of the time-of-day dimensions [`daily_series`] derives, in minutes.
///
/// Spec 3.2 §7.10 takes its rules from Agentrix §8.2, which gives 45 minutes as the floor for
/// the end of the working day.
pub const ACTIVITY_MINUTE_FLOOR: f64 = 45.0;

// ── Statistics ──────────────────────────────────────────────────

/// Returns the median of the finite values, or `None` when there are none (spec 3.2 §7.10).
///
/// An even count takes the mean of the two middle values. NaN and infinite values are
/// ignored.
///
/// # Example
///
/// ```
/// use adk_eval::ledger::median;
///
/// assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
/// assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), Some(2.5));
/// assert_eq!(median(&[f64::NAN]), None);
/// ```
pub fn median(values: &[f64]) -> Option<f64> {
    let mut finite: Vec<f64> = values.iter().copied().filter(|value| value.is_finite()).collect();
    if finite.is_empty() {
        return None;
    }
    finite.sort_by(f64::total_cmp);
    let mid = finite.len() / 2;
    if finite.len().is_multiple_of(2) {
        // Halving each value first cannot overflow.
        Some(finite[mid - 1] / 2.0 + finite[mid] / 2.0)
    } else {
        Some(finite[mid])
    }
}

/// Returns the median absolute deviation of the finite values from their median, or `None`
/// when there are none (spec 3.2 §7.10).
///
/// The MAD is unscaled: the deviation test multiplies it by k = 2.5 directly. NaN and
/// infinite values are ignored.
///
/// # Example
///
/// ```
/// use adk_eval::ledger::mad;
///
/// // Median 2; absolute deviations 1, 0, 1, 7; their median is 1.
/// assert_eq!(mad(&[1.0, 2.0, 3.0, 9.0]), Some(1.0));
/// assert_eq!(mad(&[5.0, 5.0, 5.0]), Some(0.0));
/// ```
pub fn mad(values: &[f64]) -> Option<f64> {
    let center = median(values)?;
    let deviations: Vec<f64> = values
        .iter()
        .filter(|value| value.is_finite())
        .map(|value| (value - center).abs())
        .collect();
    median(&deviations)
}

/// Whether a day is a weekday or a weekend day; baselines are kept per class (spec 3.2 §7.10).
///
/// # Example
///
/// ```
/// use adk_eval::ledger::DayClass;
/// use chrono::NaiveDate;
///
/// let saturday = NaiveDate::from_ymd_opt(2026, 10, 24).unwrap();
/// assert_eq!(DayClass::of(saturday), DayClass::Weekend);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DayClass {
    /// Monday to Friday.
    Weekday,
    /// Saturday and Sunday.
    Weekend,
}

impl DayClass {
    /// Both classes, weekday first.
    pub const ALL: [DayClass; 2] = [DayClass::Weekday, DayClass::Weekend];

    /// Returns the class of `date`.
    pub fn of(date: NaiveDate) -> Self {
        match date.weekday() {
            Weekday::Sat | Weekday::Sun => DayClass::Weekend,
            Weekday::Mon | Weekday::Tue | Weekday::Wed | Weekday::Thu | Weekday::Fri => {
                DayClass::Weekday
            }
        }
    }
}

/// One day's value of one dimension (spec 3.2 §7.10).
///
/// A series holds at most one value per day; a day without a value is absent, not zero.
///
/// # Example
///
/// ```
/// use adk_eval::ledger::DailyValue;
/// use chrono::NaiveDate;
///
/// let point = DailyValue::new(NaiveDate::from_ymd_opt(2026, 10, 22).unwrap(), 1110.0);
/// assert_eq!(point.value, 1110.0);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyValue {
    /// The day, in the user's local calendar.
    pub date: NaiveDate,
    /// The dimension's value on that day.
    pub value: f64,
}

impl DailyValue {
    /// Creates a daily value.
    pub fn new(date: NaiveDate, value: f64) -> Self {
        Self { date, value }
    }
}

/// The median and MAD of one dimension and day class over a 28-day window (spec 3.2 §7.10).
///
/// # Example
///
/// ```
/// use adk_eval::ledger::{DailyValue, DayClass, baseline};
/// use chrono::{Days, NaiveDate};
///
/// let end = NaiveDate::from_ymd_opt(2026, 10, 9).unwrap(); // a Friday
/// let series: Vec<DailyValue> =
///     (0..28).map(|back| DailyValue::new(end - Days::new(back), 100.0)).collect();
/// let weekdays = baseline(&series, DayClass::Weekday, end).unwrap();
/// assert_eq!((weekdays.median, weekdays.mad, weekdays.samples), (100.0, 0.0, 20));
/// // MAD zero: the floor alone decides.
/// assert!(weekdays.deviates(110.0, 2.5, 5.0));
/// assert!(!weekdays.deviates(110.0, 2.5, 10.0));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DimensionBaseline {
    /// The day class the baseline covers.
    pub class: DayClass,
    /// The last day of the window; the window is the 28 days ending here.
    pub window_end: NaiveDate,
    /// The median of the class's values in the window.
    pub median: f64,
    /// The median absolute deviation of those values.
    pub mad: f64,
    /// How many values the window held.
    pub samples: usize,
}

impl DimensionBaseline {
    /// Returns `max(k·MAD, floor)`, the largest distance from the median that is not a
    /// deviation.
    pub fn threshold(&self, k: f64, floor: f64) -> f64 {
        (k * self.mad).max(floor)
    }

    /// Returns `true` when `|recent − median| > max(k·MAD, floor)`.
    pub fn deviates(&self, recent: f64, k: f64, floor: f64) -> bool {
        (recent - self.median).abs() > self.threshold(k, floor)
    }
}

/// Returns the baseline of `class` over the 28 days ending on `window_end`, or `None` when
/// the window holds no value of that class (spec 3.2 §7.10).
pub fn baseline(
    series: &[DailyValue],
    class: DayClass,
    window_end: NaiveDate,
) -> Option<DimensionBaseline> {
    let values = window_values(series, class, window_end, BASELINE_WINDOW_DAYS);
    Some(DimensionBaseline {
        class,
        window_end,
        median: median(&values)?,
        mad: mad(&values)?,
        samples: values.iter().filter(|value| value.is_finite()).count(),
    })
}

/// How far a dimension's baseline has learned: days with a value in its window against the
/// 14 the warm-up requires (spec 3.2 §7.10).
///
/// # Example
///
/// ```
/// use adk_eval::ledger::WarmUp;
///
/// assert!(!WarmUp { observed_days: 13, required_days: 14 }.is_complete());
/// assert!(WarmUp { observed_days: 14, required_days: 14 }.is_complete());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WarmUp {
    /// Distinct days with a value in the baseline window, both day classes together.
    pub observed_days: u32,
    /// Days the warm-up requires, [`WARM_UP_DAYS`].
    pub required_days: u32,
}

impl WarmUp {
    /// Returns `true` once the baseline has learned from enough days to be compared against.
    pub fn is_complete(&self) -> bool {
        self.observed_days >= self.required_days
    }
}

/// Returns the warm-up of a dimension whose baseline window ends on `window_end`
/// (spec 3.2 §7.10).
pub fn warm_up(series: &[DailyValue], window_end: NaiveDate) -> WarmUp {
    let start = window_start(window_end, BASELINE_WINDOW_DAYS);
    let days: BTreeSet<NaiveDate> = series
        .iter()
        .filter(|point| point.value.is_finite() && point.date >= start && point.date <= window_end)
        .map(|point| point.date)
        .collect();
    WarmUp {
        observed_days: u32::try_from(days.len()).unwrap_or(u32::MAX),
        required_days: WARM_UP_DAYS,
    }
}

/// Returns the last day of the baseline window for a test on `as_of`: the day before the
/// 14-day comparison window starts (spec 3.2 §7.10).
pub fn baseline_window_end(as_of: NaiveDate) -> NaiveDate {
    let longest = RECENT_WINDOW_DAYS.iter().copied().max().unwrap_or(0);
    as_of.checked_sub_days(Days::new(u64::from(longest))).unwrap_or(NaiveDate::MIN)
}

/// The deviation test's settings: k and the floor of each dimension (spec 3.2 §7.10).
///
/// The default sets k = 2.5, a floor of [`ACTIVITY_MINUTE_FLOOR`] for the two time-of-day
/// dimensions [`daily_series`] derives, and a floor of 0 for every other dimension.
///
/// # Example
///
/// ```
/// use adk_eval::ledger::DeviationConfig;
///
/// let config = DeviationConfig::default().with_floor("tool_call.count", 2.0);
/// assert_eq!(config.floor("tool_call.count"), 2.0);
/// assert_eq!(config.floor("activity.last_minute"), 45.0);
/// assert_eq!(config.floor("approval.count"), 0.0);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviationConfig {
    /// The MAD multiplier, [`DEVIATION_K`] by default.
    pub k: f64,
    /// Floors by dimension name.
    pub floors: BTreeMap<String, f64>,
    /// The floor of a dimension without its own.
    pub default_floor: f64,
}

impl Default for DeviationConfig {
    fn default() -> Self {
        Self {
            k: DEVIATION_K,
            floors: BTreeMap::from([
                (ACTIVITY_FIRST_MINUTE.to_string(), ACTIVITY_MINUTE_FLOOR),
                (ACTIVITY_LAST_MINUTE.to_string(), ACTIVITY_MINUTE_FLOOR),
            ]),
            default_floor: 0.0,
        }
    }
}

impl DeviationConfig {
    /// Sets the floor of one dimension.
    pub fn with_floor(mut self, dimension: impl Into<String>, floor: f64) -> Self {
        self.floors.insert(dimension.into(), floor);
        self
    }

    /// Sets the floor of every dimension without its own.
    pub fn with_default_floor(mut self, floor: f64) -> Self {
        self.default_floor = floor;
        self
    }

    /// Returns the floor of `dimension`.
    pub fn floor(&self, dimension: &str) -> f64 {
        self.floors.get(dimension).copied().unwrap_or(self.default_floor)
    }
}

/// One comparison window of one day class (spec 3.2 §7.10).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowTest {
    /// The window's length in days, ending on the day under test.
    pub days: u32,
    /// The median of the class's values in the window; `None` when it holds none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recent_median: Option<f64>,
    /// How many values the window held.
    pub samples: usize,
    /// Whether the window's median is further from the baseline than the threshold.
    pub deviates: bool,
}

/// The deviation test of one dimension and day class (spec 3.2 §7.10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassDeviation {
    /// The day class.
    pub class: DayClass,
    /// The class's baseline; `None` when the baseline window holds no value of the class.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<DimensionBaseline>,
    /// `max(k·MAD, floor)` for the baseline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f64>,
    /// The 7-day and the 14-day window.
    pub windows: Vec<WindowTest>,
    /// `true` when the warm-up is complete and every window deviates.
    pub deviating: bool,
}

/// The deviation test of one dimension on one day (spec 3.2 §7.10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DimensionDeviation {
    /// The dimension's name.
    pub dimension: String,
    /// The floor the test used.
    pub floor: f64,
    /// How far the dimension's baseline has learned.
    pub warm_up: WarmUp,
    /// The weekday and the weekend test.
    pub classes: Vec<ClassDeviation>,
    /// `true` when either day class deviates.
    pub deviating: bool,
}

/// Runs the deviation test for one dimension on `as_of` (spec 3.2 §7.10).
///
/// For each day class it compares the medians of the last 7 and the last 14 days, `as_of`
/// included, with the baseline of the 28 days before the 14-day window. The class deviates
/// when the warm-up is complete and both windows satisfy
/// `|recent − baseline| > max(k·MAD, floor)`; a window or a baseline without a value of the
/// class does not deviate.
///
/// # Example
///
/// ```
/// use adk_eval::ledger::{DailyValue, DeviationConfig, check_dimension};
/// use chrono::{Days, NaiveDate};
///
/// let start = NaiveDate::from_ymd_opt(2026, 8, 31).unwrap();
/// let series: Vec<DailyValue> = (0..42)
///     .map(|day| DailyValue::new(start + Days::new(day), if day < 28 { 1050.0 } else { 1170.0 }))
///     .collect();
/// let as_of = start + Days::new(41);
/// let test = check_dimension("activity.last_minute", &series, as_of, &DeviationConfig::default());
/// assert!(test.warm_up.is_complete());
/// assert!(test.deviating);
/// ```
pub fn check_dimension(
    dimension: &str,
    series: &[DailyValue],
    as_of: NaiveDate,
    config: &DeviationConfig,
) -> DimensionDeviation {
    let window_end = baseline_window_end(as_of);
    let warm_up = warm_up(series, window_end);
    let floor = config.floor(dimension);
    let classes: Vec<ClassDeviation> = DayClass::ALL
        .iter()
        .map(|&class| {
            let baseline = baseline(series, class, window_end);
            let windows: Vec<WindowTest> = RECENT_WINDOW_DAYS
                .iter()
                .map(|&days| {
                    let values = window_values(series, class, as_of, days);
                    let recent_median = median(&values);
                    let deviates = match (recent_median, &baseline) {
                        (Some(recent), Some(baseline)) => {
                            baseline.deviates(recent, config.k, floor)
                        }
                        (None, _) | (_, None) => false,
                    };
                    WindowTest {
                        days,
                        recent_median,
                        samples: values.iter().filter(|value| value.is_finite()).count(),
                        deviates,
                    }
                })
                .collect();
            ClassDeviation {
                class,
                threshold: baseline.map(|baseline| baseline.threshold(config.k, floor)),
                baseline,
                deviating: warm_up.is_complete() && windows.iter().all(|window| window.deviates),
                windows,
            }
        })
        .collect();
    DimensionDeviation {
        dimension: dimension.to_string(),
        floor,
        warm_up,
        deviating: classes.iter().any(|class| class.deviating),
        classes,
    }
}

/// The deviation test of every dimension on one day (spec 3.2 §7.10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviationReport {
    /// The day under test, the last day of both comparison windows.
    pub as_of: NaiveDate,
    /// One test per dimension, sorted by name.
    pub dimensions: Vec<DimensionDeviation>,
}

impl DeviationReport {
    /// Returns the names of the deviating dimensions, sorted.
    pub fn deviating(&self) -> Vec<&str> {
        self.dimensions
            .iter()
            .filter(|dimension| dimension.deviating)
            .map(|dimension| dimension.dimension.as_str())
            .collect()
    }
}

/// Runs [`check_dimension`] for every dimension of `series` on `as_of` (spec 3.2 §7.10).
pub fn check_series(
    series: &BTreeMap<String, Vec<DailyValue>>,
    as_of: NaiveDate,
    config: &DeviationConfig,
) -> DeviationReport {
    DeviationReport {
        as_of,
        dimensions: series
            .iter()
            .map(|(dimension, values)| check_dimension(dimension, values, as_of, config))
            .collect(),
    }
}

// ── Meaningful drift and cooldown ───────────────────────────────

/// Returns `true` when deviating dimensions with these run lengths, in days, are a
/// meaningful drift: two dimensions, or one that has deviated for 10 days (spec 3.2 §7.10).
///
/// # Example
///
/// ```
/// use adk_eval::ledger::is_meaningful;
///
/// assert!(!is_meaningful(&[9]));
/// assert!(is_meaningful(&[10]));
/// assert!(is_meaningful(&[1, 1]));
/// assert!(!is_meaningful(&[]));
/// ```
pub fn is_meaningful(run_days: &[u32]) -> bool {
    run_days.len() >= MEANINGFUL_DIMENSIONS
        || run_days.iter().any(|&days| days >= MEANINGFUL_RUN_DAYS)
}

/// The drift a caller carries from one daily tick to the next: when each deviating
/// dimension's run started, and when each dimension was last observed (spec 3.2 §7.10).
///
/// [`DriftState::advance`] is a pure transition: it returns the next state and leaves this one
/// unchanged, so the kernel can persist the state between ticks.
///
/// # Example
///
/// ```
/// use adk_eval::ledger::DriftState;
/// use chrono::{Days, NaiveDate};
///
/// let day = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
/// // Two dimensions deviate: meaningful, so both are observed.
/// let step = DriftState::default().advance(day, &["activity.last_minute", "tool_call.count"]);
/// assert_eq!(step.observed, ["activity.last_minute", "tool_call.count"]);
///
/// // Six days later both are still in their cooldown.
/// let later = step.state.advance(day + Days::new(6), &["activity.last_minute", "tool_call.count"]);
/// assert!(later.observed.is_empty());
///
/// // On the seventh day the cooldown is over.
/// let again = later.state.advance(day + Days::new(7), &["activity.last_minute", "tool_call.count"]);
/// assert_eq!(again.observed.len(), 2);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriftState {
    /// The first day of each deviating dimension's current run of deviating days.
    pub deviating_since: BTreeMap<String, NaiveDate>,
    /// The day an observation last included each dimension.
    pub last_observed: BTreeMap<String, NaiveDate>,
}

/// A deviating dimension's current run (spec 3.2 §7.10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriftRun {
    /// The dimension's name.
    pub dimension: String,
    /// The first day of the run.
    pub since: NaiveDate,
    /// The run's length in calendar days, both ends included.
    pub days: u32,
    /// `true` when an observation included the dimension less than 7 days ago.
    pub cooling_down: bool,
}

/// The result of one [`DriftState::advance`] (spec 3.2 §7.10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriftStep {
    /// The state to carry to the next tick.
    pub state: DriftState,
    /// Every deviating dimension's run, sorted by name, cooling down or not.
    pub runs: Vec<DriftRun>,
    /// The dimensions an observation on this day includes, sorted; empty when there is no
    /// observation.
    pub observed: Vec<String>,
}

impl DriftState {
    /// Returns `true` when an observation included `dimension` less than 7 days before `day`.
    pub fn in_cooldown(&self, dimension: &str, day: NaiveDate) -> bool {
        self.last_observed
            .get(dimension)
            .is_some_and(|last| (day - *last).num_days() < i64::from(COOLDOWN_DAYS))
    }

    /// Records the dimensions deviating on `day` and decides whether `day` has an
    /// observation (spec 3.2 §7.10).
    ///
    /// A deviating dimension starts a run or continues its run; a dimension not in
    /// `deviating` ends its run. The dimensions out of their cooldown are the candidates, and
    /// when their runs are meaningful ([`is_meaningful`]) the observation includes every
    /// candidate and starts its cooldown. Calling it twice for the same day observes nothing
    /// the second time.
    pub fn advance(&self, day: NaiveDate, deviating: &[&str]) -> DriftStep {
        let mut names: Vec<&str> = deviating.to_vec();
        names.sort_unstable();
        names.dedup();

        let mut state = DriftState {
            deviating_since: BTreeMap::new(),
            last_observed: self.last_observed.clone(),
        };
        let runs: Vec<DriftRun> = names
            .into_iter()
            .map(|name| {
                let since = self
                    .deviating_since
                    .get(name)
                    .copied()
                    .filter(|since| *since <= day)
                    .unwrap_or(day);
                state.deviating_since.insert(name.to_string(), since);
                DriftRun {
                    dimension: name.to_string(),
                    since,
                    days: u32::try_from((day - since).num_days() + 1).unwrap_or(u32::MAX),
                    cooling_down: self.in_cooldown(name, day),
                }
            })
            .collect();

        let candidates: Vec<&DriftRun> = runs.iter().filter(|run| !run.cooling_down).collect();
        let candidate_days: Vec<u32> = candidates.iter().map(|run| run.days).collect();
        let observed: Vec<String> = if is_meaningful(&candidate_days) {
            candidates.iter().map(|run| run.dimension.clone()).collect()
        } else {
            Vec::new()
        };
        for dimension in &observed {
            state.last_observed.insert(dimension.clone(), day);
        }
        DriftStep { state, runs, observed }
    }
}

// ── Regression gate ─────────────────────────────────────────────

/// The state of a whole ledger on its last day, as the `cargo adk eval --ledger` gate reads
/// it (spec 3.2 §7.10, §10 E1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerAssessment {
    /// The first day with a value in any dimension.
    pub first_day: NaiveDate,
    /// The last day with a value in any dimension; the day under test.
    pub last_day: NaiveDate,
    /// The deviation test on the last day.
    pub report: DeviationReport,
    /// The runs of the dimensions deviating on the last day, sorted by name.
    pub runs: Vec<DriftRun>,
    /// `true` when those runs are meaningful, cooldown not applied.
    pub meaningful: bool,
}

/// Replays the daily deviation test over every day of `series` and assesses its last day
/// (spec 3.2 §7.10, §10 E1). Returns `None` when `series` holds no value.
///
/// Each calendar day from the first to the last runs [`check_series`] and advances a
/// [`DriftState`], so each run length is what a daily tick would have recorded. The
/// assessment is meaningful when the last day's runs are ([`is_meaningful`]); the cooldown
/// is not applied, since a regression gate keeps failing while a drift lasts.
pub fn assess(
    series: &BTreeMap<String, Vec<DailyValue>>,
    config: &DeviationConfig,
) -> Option<LedgerAssessment> {
    let dates = || series.values().flatten().map(|point| point.date);
    let first_day = dates().min()?;
    let last_day = dates().max()?;

    let mut state = DriftState::default();
    let mut runs = Vec::new();
    let mut report = check_series(series, first_day, config);
    for day in first_day.iter_days().take_while(|day| *day <= last_day) {
        report = check_series(series, day, config);
        let step = state.advance(day, &report.deviating());
        state = step.state;
        runs = step.runs;
    }
    let run_days: Vec<u32> = runs.iter().map(|run| run.days).collect();
    Some(LedgerAssessment {
        first_day,
        last_day,
        report,
        meaningful: is_meaningful(&run_days),
        runs,
    })
}

// ── Ledger export ───────────────────────────────────────────────

/// The minute of the day of a day's first ledger row, a dimension of [`daily_series`].
pub const ACTIVITY_FIRST_MINUTE: &str = "activity.first_minute";

/// The minute of the day of a day's last ledger row, a dimension of [`daily_series`].
pub const ACTIVITY_LAST_MINUTE: &str = "activity.last_minute";

/// One row of an exported activity ledger, with the columns of spec 3.2 §7.10.
///
/// A ledger is content-free: a row records what kind of thing happened, when, for how long
/// and to which hashed subject — never a payload, a prompt or a secret. An export is one
/// camelCase JSON object per line; fields this type does not know are ignored.
///
/// ```text
/// {"ts":"2026-10-22T19:20:04+03:00","partition":"work","agent":"assistant","kind":"tool_call","effect":"read_only","durationMs":182,"subjectHash":"hmac-sha256:5f2c…","meta":{"attempt":1},"traceId":"4bf92f35…"}
/// ```
///
/// # Example
///
/// ```
/// use adk_eval::ledger::LedgerRow;
///
/// let row: LedgerRow = serde_json::from_str(
///     r#"{"ts":"2026-10-22T19:20:04+03:00","partition":"work","agent":"assistant","kind":"wake"}"#,
/// )
/// .unwrap();
/// assert_eq!(row.kind, "wake");
/// assert_eq!(row.duration_ms, None);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerRow {
    /// When the row was written, RFC 3339 with the offset of the user's local time; its day
    /// and time of day are read in that offset.
    pub ts: DateTime<FixedOffset>,
    /// The partition the activity belongs to.
    pub partition: String,
    /// The agent that acted.
    pub agent: String,
    /// What happened: `intent`, `tool_call`, `approval`, `rejection`, `observation`, `wake`,
    /// `proposal`, or another kind the kernel defines.
    pub kind: String,
    /// The effect class of the action, such as `read_only`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<String>,
    /// How long the action took.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// An HMAC of the action's subject, never the subject itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_hash: Option<String>,
    /// Small, schema-checked metadata; numbers only feed the statistics.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub meta: serde_json::Map<String, serde_json::Value>,
    /// The trace the row belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

/// A line of a ledger export that is not a [`LedgerRow`] (spec 3.2 §7.10).
#[derive(Debug, thiserror::Error)]
#[error(
    "ledger line {line} is not a ledger row: {source}. Each line must be one JSON object with \
     ts (RFC 3339 with an offset), partition, agent and kind, and optionally effect, \
     durationMs, subjectHash, meta and traceId"
)]
pub struct LedgerParseError {
    /// The one-based line number.
    pub line: usize,
    /// Why the line did not parse.
    #[source]
    pub source: serde_json::Error,
}

/// Parses a ledger export, one JSON object per line; blank lines are skipped
/// (spec 3.2 §7.10).
///
/// # Errors
///
/// Returns [`LedgerParseError`] for the first line that is not a [`LedgerRow`].
///
/// # Example
///
/// ```
/// use adk_eval::ledger::parse_jsonl;
///
/// let export = r#"
/// {"ts":"2026-10-22T09:02:00+03:00","partition":"default","agent":"assistant","kind":"intent"}
///
/// {"ts":"2026-10-22T09:03:10+03:00","partition":"default","agent":"assistant","kind":"tool_call","durationMs":140}
/// "#;
/// assert_eq!(parse_jsonl(export).unwrap().len(), 2);
/// assert_eq!(parse_jsonl("{}").unwrap_err().line, 1);
/// ```
pub fn parse_jsonl(text: &str) -> Result<Vec<LedgerRow>, LedgerParseError> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line)
                .map_err(|source| LedgerParseError { line: index + 1, source })
        })
        .collect()
}

/// Derives one daily series per dimension from ledger rows (spec 3.2 §7.10).
///
/// A row's day and time of day are read in the offset of its timestamp. Days without a row
/// have no value in any dimension.
///
/// | Dimension | Daily value |
/// |-----------|-------------|
/// | `activity.first_minute` | Minute of the day of the first row |
/// | `activity.last_minute` | Minute of the day of the last row |
/// | `<kind>.count` | Rows of that kind; 0 on a day with rows of other kinds only |
/// | `<kind>.duration_ms` | Median `durationMs` of that kind's rows that carry one |
/// | `<kind>.meta.<key>` | Sum of that kind's numeric top-level `meta` values under `key` |
///
/// # Example
///
/// ```
/// use adk_eval::ledger::{daily_series, parse_jsonl};
///
/// let rows = parse_jsonl(
///     r#"{"ts":"2026-10-22T09:00:00+03:00","partition":"default","agent":"a","kind":"tool_call","durationMs":100}
/// {"ts":"2026-10-22T17:30:00+03:00","partition":"default","agent":"a","kind":"tool_call","durationMs":300}"#,
/// )
/// .unwrap();
/// let series = daily_series(&rows);
/// assert_eq!(series["tool_call.count"][0].value, 2.0);
/// assert_eq!(series["tool_call.duration_ms"][0].value, 200.0);
/// assert_eq!(series["activity.last_minute"][0].value, 17.0 * 60.0 + 30.0);
/// ```
pub fn daily_series(rows: &[LedgerRow]) -> BTreeMap<String, Vec<DailyValue>> {
    let mut days: BTreeMap<NaiveDate, Vec<&LedgerRow>> = BTreeMap::new();
    for row in rows {
        days.entry(row.ts.date_naive()).or_default().push(row);
    }
    let kinds: BTreeSet<&str> = rows.iter().map(|row| row.kind.as_str()).collect();

    let mut series: BTreeMap<String, Vec<DailyValue>> = BTreeMap::new();
    let mut push = |dimension: String, date: NaiveDate, value: f64| {
        series.entry(dimension).or_default().push(DailyValue::new(date, value));
    };
    for (&date, day_rows) in &days {
        let minutes: Vec<f64> = day_rows
            .iter()
            .map(|row| f64::from(row.ts.time().num_seconds_from_midnight()) / 60.0)
            .collect();
        let first = minutes.iter().copied().fold(f64::INFINITY, f64::min);
        let last = minutes.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        push(ACTIVITY_FIRST_MINUTE.to_string(), date, first);
        push(ACTIVITY_LAST_MINUTE.to_string(), date, last);

        for kind in &kinds {
            let of_kind: Vec<&LedgerRow> =
                day_rows.iter().copied().filter(|row| row.kind == *kind).collect();
            push(format!("{kind}.count"), date, of_kind.len() as f64);

            let durations: Vec<f64> =
                of_kind.iter().filter_map(|row| row.duration_ms).map(|ms| ms as f64).collect();
            if let Some(duration) = median(&durations) {
                push(format!("{kind}.duration_ms"), date, duration);
            }

            let mut sums: BTreeMap<&str, f64> = BTreeMap::new();
            for row in &of_kind {
                for (key, value) in &row.meta {
                    if let Some(number) = value.as_f64() {
                        *sums.entry(key.as_str()).or_default() += number;
                    }
                }
            }
            for (key, sum) in sums {
                push(format!("{kind}.meta.{key}"), date, sum);
            }
        }
    }
    series
}

/// The values of `class` in the `days` days ending on `end`, `end` included.
fn window_values(series: &[DailyValue], class: DayClass, end: NaiveDate, days: u32) -> Vec<f64> {
    let start = window_start(end, days);
    series
        .iter()
        .filter(|point| {
            point.date >= start && point.date <= end && DayClass::of(point.date) == class
        })
        .map(|point| point.value)
        .collect()
}

/// The first day of the `days`-day window ending on `end`.
fn window_start(end: NaiveDate, days: u32) -> NaiveDate {
    end.checked_sub_days(Days::new(u64::from(days.saturating_sub(1)))).unwrap_or(NaiveDate::MIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    /// Monday 2026-08-31.
    fn start() -> NaiveDate {
        date(2026, 8, 31)
    }

    fn day(offset: u64) -> NaiveDate {
        start() + Days::new(offset)
    }

    /// One value per day for `days` days from [`start`], from `value(offset)`.
    fn series(days: u64, value: impl Fn(u64) -> f64) -> Vec<DailyValue> {
        (0..days).map(|offset| DailyValue::new(day(offset), value(offset))).collect()
    }

    #[test]
    fn median_and_mad_of_odd_even_and_empty_inputs() {
        assert_eq!(median(&[]), None);
        assert_eq!(mad(&[]), None);
        assert_eq!(median(&[7.0]), Some(7.0));
        assert_eq!(mad(&[7.0]), Some(0.0));
        assert_eq!(median(&[5.0, 1.0, 3.0]), Some(3.0));
        assert_eq!(median(&[1.0, 4.0, 2.0, 3.0]), Some(2.5));
        assert_eq!(mad(&[1.0, 2.0, 3.0, 4.0, 100.0]), Some(1.0));
        assert_eq!(median(&[1.0, f64::NAN, 3.0, f64::INFINITY]), Some(2.0));
    }

    #[test]
    fn day_classes_split_saturday_and_sunday_from_the_week() {
        let classes: Vec<DayClass> = (0..7).map(|offset| DayClass::of(day(offset))).collect();
        assert_eq!(
            classes,
            [
                DayClass::Weekday,
                DayClass::Weekday,
                DayClass::Weekday,
                DayClass::Weekday,
                DayClass::Weekday,
                DayClass::Weekend,
                DayClass::Weekend,
            ]
        );
    }

    #[test]
    fn the_baseline_window_ends_before_the_fourteen_day_window() {
        let as_of = day(41);
        assert_eq!(baseline_window_end(as_of), day(27));
        // A drift confined to the comparison windows leaves the baseline untouched.
        let points = series(42, |offset| if offset < 28 { 10.0 } else { 99.0 });
        let weekdays = baseline(&points, DayClass::Weekday, baseline_window_end(as_of)).unwrap();
        assert_eq!(
            weekdays,
            DimensionBaseline {
                class: DayClass::Weekday,
                window_end: day(27),
                median: 10.0,
                mad: 0.0,
                samples: 20,
            }
        );
    }

    #[test]
    fn warm_up_completes_on_the_fourteenth_baseline_day() {
        // Values on days 0..n, then a shift over the whole comparison window that would
        // deviate once the baseline is warm.
        let as_of = |baseline_days: u64| day(baseline_days - 1 + 14);
        let shifted = |baseline_days: u64| {
            series(
                baseline_days + 14,
                move |offset| if offset < baseline_days { 10.0 } else { 50.0 },
            )
        };
        let config = DeviationConfig::default();

        let day13 = check_dimension("x", &shifted(13), as_of(13), &config);
        assert_eq!(day13.warm_up, WarmUp { observed_days: 13, required_days: 14 });
        assert!(!day13.deviating);
        assert!(day13.classes.iter().all(|class| !class.deviating));

        let day14 = check_dimension("x", &shifted(14), as_of(14), &config);
        assert_eq!(day14.warm_up, WarmUp { observed_days: 14, required_days: 14 });
        assert!(day14.deviating);
    }

    #[test]
    fn warm_up_counts_days_not_values_and_ignores_days_outside_the_window() {
        let mut points = series(14, |_| 1.0);
        points.push(DailyValue::new(day(3), 2.0));
        assert_eq!(warm_up(&points, day(13)).observed_days, 14);
        // 28 days later only the last of those days is still in the window.
        assert_eq!(warm_up(&points, day(13 + 27)).observed_days, 1);
    }

    #[test]
    fn mad_zero_leaves_the_floor_to_decide() {
        let constant = series(28, |_| 100.0);
        let weekdays = baseline(&constant, DayClass::Weekday, day(27)).unwrap();
        assert_eq!(weekdays.mad, 0.0);
        assert!(weekdays.deviates(110.0, DEVIATION_K, 0.0));
        assert!(weekdays.deviates(110.0, DEVIATION_K, 9.99));
        // The test is strict: a difference equal to the floor is not a deviation.
        assert!(!weekdays.deviates(110.0, DEVIATION_K, 10.0));
        assert!(!weekdays.deviates(100.0, DEVIATION_K, 0.0));
    }

    #[test]
    fn floors_raise_the_threshold_over_k_times_mad() {
        // MAD 1: k·MAD is 2.5, so a floor of 4 decides and a floor of 2 does not.
        let alternating = series(28, |offset| if offset % 2 == 0 { 99.0 } else { 101.0 });
        let weekdays = baseline(&alternating, DayClass::Weekday, day(27)).unwrap();
        assert_eq!(weekdays.mad, 1.0);
        assert_eq!(weekdays.threshold(DEVIATION_K, 2.0), 2.5);
        assert_eq!(weekdays.threshold(DEVIATION_K, 4.0), 4.0);
        assert!(weekdays.deviates(103.0, DEVIATION_K, 2.0));
        assert!(!weekdays.deviates(103.0, DEVIATION_K, 4.0));
    }

    #[test]
    fn weekday_and_weekend_keep_separate_baselines() {
        // Weekdays at 100, weekends at 0: pooled, the weekend days would look like a drift.
        let weekly = |offset: u64| match DayClass::of(day(offset)) {
            DayClass::Weekday => 100.0,
            DayClass::Weekend => 0.0,
        };
        let steady = series(42, weekly);
        let test = check_dimension("x", &steady, day(41), &DeviationConfig::default());
        assert!(!test.deviating);
        assert_eq!(
            test.classes.iter().map(|class| class.baseline.unwrap().median).collect::<Vec<_>>(),
            [100.0, 0.0]
        );

        // Only the weekends move: the weekend class deviates and the weekday class does not.
        let weekend_drift = series(42, |offset| match DayClass::of(day(offset)) {
            DayClass::Weekday => 100.0,
            DayClass::Weekend if offset >= 28 => 60.0,
            DayClass::Weekend => 0.0,
        });
        let test = check_dimension("x", &weekend_drift, day(41), &DeviationConfig::default());
        assert!(test.deviating);
        assert_eq!(
            test.classes.iter().map(|class| (class.class, class.deviating)).collect::<Vec<_>>(),
            [(DayClass::Weekday, false), (DayClass::Weekend, true)]
        );
    }

    #[test]
    fn a_single_busy_week_does_not_deviate() {
        // The last 7 days move by 40; the 14-day window's median moves by 20, under the floor.
        let busy_week = series(42, |offset| if offset >= 35 { 50.0 } else { 10.0 });
        let config = DeviationConfig::default().with_floor("x", 25.0);
        let test = check_dimension("x", &busy_week, day(41), &config);
        let weekday = &test.classes[0];
        assert_eq!(
            weekday.windows.iter().map(|window| window.deviates).collect::<Vec<_>>(),
            [true, false]
        );
        assert!(!test.deviating);
    }

    #[test]
    fn a_class_without_values_does_not_deviate() {
        // Weekday-only activity: the weekend class has no baseline and no window values.
        let weekdays_only: Vec<DailyValue> =
            series(42, |offset| if offset < 28 { 1.0 } else { 9.0 })
                .into_iter()
                .filter(|point| DayClass::of(point.date) == DayClass::Weekday)
                .collect();
        let test = check_dimension("x", &weekdays_only, day(41), &DeviationConfig::default());
        let weekend = &test.classes[1];
        assert_eq!(weekend.baseline, None);
        assert!(weekend.windows.iter().all(|window| window.recent_median.is_none()));
        assert!(!weekend.deviating);
        assert!(test.deviating);
    }

    #[test]
    fn meaningful_needs_two_dimensions_or_ten_days() {
        assert!(!is_meaningful(&[]));
        assert!(!is_meaningful(&[1]));
        assert!(!is_meaningful(&[9]));
        assert!(is_meaningful(&[10]));
        assert!(is_meaningful(&[1, 2]));
    }

    #[test]
    fn a_single_dimension_is_observed_on_its_tenth_day() {
        let mut state = DriftState::default();
        for offset in 0..9 {
            let step = state.advance(day(offset), &["x"]);
            assert!(step.observed.is_empty(), "day {}", offset + 1);
            state = step.state;
        }
        let tenth = state.advance(day(9), &["x"]);
        assert_eq!(tenth.observed, ["x"]);
        assert_eq!(
            tenth.runs,
            [DriftRun { dimension: "x".into(), since: day(0), days: 10, cooling_down: false }]
        );
    }

    #[test]
    fn a_day_without_the_deviation_ends_the_run() {
        let step = DriftState::default().advance(day(0), &["x"]);
        let step = step.state.advance(day(1), &[]);
        assert!(step.state.deviating_since.is_empty());
        let step = step.state.advance(day(2), &["x"]);
        assert_eq!(step.runs[0].since, day(2));
        assert_eq!(step.runs[0].days, 1);
    }

    #[test]
    fn the_cooldown_lasts_seven_days_per_dimension() {
        let observed = DriftState::default().advance(day(0), &["a", "b"]);
        assert_eq!(observed.observed, ["a", "b"]);

        // Day 6: both cooling down; a new dimension alone is not meaningful.
        let day6 = observed.state.advance(day(6), &["a", "b", "c"]);
        assert!(day6.observed.is_empty());
        assert_eq!(
            day6.runs.iter().map(|run| run.cooling_down).collect::<Vec<_>>(),
            [true, true, false]
        );

        // Day 7: the cooldown is over and all three are candidates.
        let day7 = day6.state.advance(day(7), &["a", "b", "c"]);
        assert_eq!(day7.observed, ["a", "b", "c"]);
        assert_eq!(day7.state.last_observed.get("a"), Some(&day(7)));
    }

    #[test]
    fn advancing_twice_on_one_day_observes_once() {
        let first = DriftState::default().advance(day(0), &["a", "b"]);
        let second = first.state.advance(day(0), &["a", "b"]);
        assert_eq!(first.observed.len(), 2);
        assert!(second.observed.is_empty());
        assert_eq!(first.state, second.state);
    }

    #[test]
    fn advance_leaves_the_state_it_was_called_on_unchanged() {
        let state = DriftState::default().advance(day(0), &["a"]).state;
        let before = state.clone();
        let _ = state.advance(day(1), &["a", "b"]);
        assert_eq!(state, before);
    }

    #[test]
    fn assess_counts_runs_across_the_whole_ledger() {
        let drift = series(42, |offset| if offset < 28 { 10.0 } else { 50.0 });
        let dims = BTreeMap::from([("x".to_string(), drift)]);
        let assessment = assess(&dims, &DeviationConfig::default()).unwrap();
        assert_eq!((assessment.first_day, assessment.last_day), (day(0), day(41)));
        assert_eq!(assessment.report.as_of, day(41));
        assert_eq!(assessment.runs.len(), 1);
        let run = &assessment.runs[0];
        assert_eq!(run.days, u32::try_from((day(41) - run.since).num_days() + 1).unwrap());
        assert_eq!(assessment.meaningful, run.days >= MEANINGFUL_RUN_DAYS);
        assert_eq!(assess(&BTreeMap::new(), &DeviationConfig::default()), None);
    }

    #[test]
    fn rows_parse_from_camel_case_and_ignore_unknown_fields() {
        let rows = parse_jsonl(
            r#"{"ts":"2026-10-22T19:20:04+03:00","partition":"work","agent":"assistant","kind":"tool_call","effect":"read_only","durationMs":182,"subjectHash":"hmac-sha256:5f2c","meta":{"attempt":1},"traceId":"4bf9","seq":7}"#,
        )
        .unwrap();
        assert_eq!(
            rows,
            [LedgerRow {
                ts: DateTime::parse_from_rfc3339("2026-10-22T19:20:04+03:00").unwrap(),
                partition: "work".into(),
                agent: "assistant".into(),
                kind: "tool_call".into(),
                effect: Some("read_only".into()),
                duration_ms: Some(182),
                subject_hash: Some("hmac-sha256:5f2c".into()),
                meta: serde_json::Map::from_iter([("attempt".to_string(), 1.into())]),
                trace_id: Some("4bf9".into()),
            }]
        );
        let json = serde_json::to_value(&rows[0]).unwrap();
        assert_eq!(json["durationMs"], 182);
        assert_eq!(json["subjectHash"], "hmac-sha256:5f2c");
        assert_eq!(json["traceId"], "4bf9");
    }

    #[test]
    fn a_bad_line_reports_its_line_number() {
        let export = "\n{\"ts\":\"2026-10-22T09:00:00Z\",\"partition\":\"p\",\"agent\":\"a\",\"kind\":\"k\"}\n{\"ts\":\"2026-10-22T09:00:00\",\"partition\":\"p\",\"agent\":\"a\",\"kind\":\"k\"}\n";
        let error = parse_jsonl(export).unwrap_err();
        assert_eq!(error.line, 3);
        assert!(error.to_string().contains("RFC 3339 with an offset"), "{error}");
    }

    #[test]
    fn daily_series_reads_days_and_times_in_the_row_offset() {
        let rows = parse_jsonl(
            r#"{"ts":"2026-10-22T23:30:00+03:00","partition":"p","agent":"a","kind":"wake"}
{"ts":"2026-10-22T08:15:00+03:00","partition":"p","agent":"a","kind":"tool_call","durationMs":100,"meta":{"inputTokens":10,"label":"x"}}
{"ts":"2026-10-22T09:00:00+03:00","partition":"p","agent":"a","kind":"tool_call","durationMs":300,"meta":{"inputTokens":5}}
{"ts":"2026-10-23T10:00:00+03:00","partition":"p","agent":"a","kind":"wake"}"#,
        )
        .unwrap();
        let series = daily_series(&rows);
        let values = |dimension: &str| -> Vec<(NaiveDate, f64)> {
            series[dimension].iter().map(|point| (point.date, point.value)).collect()
        };
        let (d22, d23) = (date(2026, 10, 22), date(2026, 10, 23));
        assert_eq!(
            series.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "activity.first_minute",
                "activity.last_minute",
                "tool_call.count",
                "tool_call.duration_ms",
                "tool_call.meta.inputTokens",
                "wake.count",
            ]
        );
        assert_eq!(values("activity.first_minute"), [(d22, 8.0 * 60.0 + 15.0), (d23, 600.0)]);
        // 23:30 in the row's own offset, which is 20:30 UTC.
        assert_eq!(values("activity.last_minute"), [(d22, 23.0 * 60.0 + 30.0), (d23, 600.0)]);
        assert_eq!(values("tool_call.count"), [(d22, 2.0), (d23, 0.0)]);
        assert_eq!(values("tool_call.duration_ms"), [(d22, 200.0)]);
        assert_eq!(values("tool_call.meta.inputTokens"), [(d22, 15.0)]);
        assert_eq!(values("wake.count"), [(d22, 1.0), (d23, 1.0)]);
    }
}
