//! Queue classes (spec 3.2 §7.9).
//!
//! Priority is a queue (§6 D-5): the kernel runs one `DurableWorker` per [`QueueClass`],
//! each with its own fallback poll interval (§13 Q2). A worker sleeps until the earliest
//! `not_before` deadline in its queue, and `enqueue` and `wake` notify it, so the poll only
//! catches runs another process enqueued (§10 S3).
//!
//! Phase 2 adds attention and resources (§7.9): [`Quotas`] per partition (max running, max
//! queued, a daily spend cap), [`QuietHours`] through the run's `not_before`, cost-aware
//! admission against the partition's daily spend, and preemption of the youngest
//! background run when an interactive one finds its partition at quota.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, FixedOffset, Local, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::KernelError;

/// A priority class: the run-store queue a process waits in and the worker that claims it
/// (spec 3.2 §7.9).
///
/// The three classes of the default configuration are [`Interactive`](Self::Interactive)
/// (a human turn), [`Background`](Self::Background) (goals, triggered and scheduled work)
/// and [`Maintenance`](Self::Maintenance) (the kernel's own work, `kernel.wake` runs).
/// [`Named`](Self::Named) is any other queue. A class serializes as its queue name.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_kernel::QueueClass;
///
/// let class: QueueClass = "background".parse().unwrap();
/// assert_eq!(class, QueueClass::Background);
/// assert_eq!(class.poll_interval(), Duration::from_secs(2));
/// assert_eq!(QueueClass::Interactive.poll_interval(), Duration::from_millis(500));
/// assert_eq!(QueueClass::from_name("payments"), QueueClass::Named("payments".into()));
/// assert_eq!(serde_json::to_value(QueueClass::Maintenance).unwrap(), "maintenance");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum QueueClass {
    /// Human turns; claimed first-come with a 500 ms fallback poll.
    Interactive,
    /// Goals, triggered and scheduled processes; 2 s fallback poll.
    Background,
    /// The kernel's own work; 5 s fallback poll.
    Maintenance,
    /// Any other queue; 2 s fallback poll.
    Named(String),
}

impl QueueClass {
    /// The run-store queue name of [`Interactive`](Self::Interactive).
    pub const INTERACTIVE: &'static str = "interactive";
    /// The run-store queue name of [`Background`](Self::Background).
    pub const BACKGROUND: &'static str = "background";
    /// The run-store queue name of [`Maintenance`](Self::Maintenance).
    pub const MAINTENANCE: &'static str = "maintenance";

    /// The class of the run-store queue `name`; the three default names map to their
    /// classes and anything else is [`Named`](Self::Named).
    pub fn from_name(name: &str) -> Self {
        match name {
            Self::INTERACTIVE => Self::Interactive,
            Self::BACKGROUND => Self::Background,
            Self::MAINTENANCE => Self::Maintenance,
            other => Self::Named(other.to_string()),
        }
    }

    /// The run-store queue name the class's worker claims from.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Interactive => Self::INTERACTIVE,
            Self::Background => Self::BACKGROUND,
            Self::Maintenance => Self::MAINTENANCE,
            Self::Named(name) => name,
        }
    }

    /// The worker's fallback poll interval (spec 3.2 §7.9, §13 Q2): 500 ms for
    /// `interactive`, 2 s for `background` and named queues, 5 s for `maintenance`.
    pub fn poll_interval(&self) -> Duration {
        match self {
            Self::Interactive => Duration::from_millis(500),
            Self::Background | Self::Named(_) => Duration::from_secs(2),
            Self::Maintenance => Duration::from_secs(5),
        }
    }

    /// The default queue classes: interactive, background and maintenance (spec 3.2 §7.2).
    pub fn defaults() -> Vec<Self> {
        vec![Self::Interactive, Self::Background, Self::Maintenance]
    }
}

impl fmt::Display for QueueClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for QueueClass {
    type Err = KernelError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        let name = name.trim();
        if name.is_empty() {
            return Err(KernelError::Config {
                reason: "a queue name must not be empty; use interactive, background, \
                         maintenance or a queue the kernel configures"
                    .to_string(),
            });
        }
        Ok(Self::from_name(name))
    }
}

impl Serialize for QueueClass {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for QueueClass {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        name.parse().map_err(serde::de::Error::custom)
    }
}

/// A daily local-time window in which proactive and maintenance processes never start
/// (spec 3.2 §7.5, §7.9, K-I11); interactive and approval-woken runs always may.
///
/// The window runs from `start` up to, not including, `end`. A `start` after `end` wraps
/// midnight (`22:00`–`07:00`); equal times make an empty window. Times are local: at
/// `utc_offset_minutes` east of UTC when set, else in the host's time zone. A process that
/// would start inside the window gets `not_before` = [`release_at`](Self::release_at), so
/// its worker sleeps until the window ends. Serializes as
/// `{"start": "22:00", "end": "07:00", "utcOffsetMinutes": 180}`.
///
/// # Example
///
/// ```rust
/// use chrono::{TimeZone, Utc};
/// use adk_kernel::QuietHours;
///
/// let quiet: QuietHours = "22:00-07:00".parse().unwrap();
/// let quiet = quiet.with_utc_offset_minutes(0);
/// let night = Utc.with_ymd_and_hms(2026, 10, 23, 23, 30, 0).unwrap();
/// assert!(quiet.contains(night));
/// assert_eq!(quiet.release_at(night), Utc.with_ymd_and_hms(2026, 10, 24, 7, 0, 0).unwrap());
/// let noon = Utc.with_ymd_and_hms(2026, 10, 23, 12, 0, 0).unwrap();
/// assert!(!quiet.contains(noon));
/// assert_eq!(quiet.release_at(noon), noon);
/// assert_eq!(serde_json::to_value(&quiet).unwrap()["start"], "22:00");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct QuietHours {
    /// Local time the window starts.
    #[serde(with = "hh_mm")]
    pub start: NaiveTime,
    /// Local time the window ends (exclusive).
    #[serde(with = "hh_mm")]
    pub end: NaiveTime,
    /// Minutes east of UTC the times are in; `None` is the host's time zone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utc_offset_minutes: Option<i32>,
}

impl QuietHours {
    /// The window from `start` to `end`, in the host's time zone.
    pub fn new(start: NaiveTime, end: NaiveTime) -> Self {
        Self { start, end, utc_offset_minutes: None }
    }

    /// Reads the times at `minutes` east of UTC instead of the host's time zone.
    #[must_use]
    pub fn with_utc_offset_minutes(mut self, minutes: i32) -> Self {
        self.utc_offset_minutes = Some(minutes);
        self
    }

    fn offset(&self) -> Option<FixedOffset> {
        self.utc_offset_minutes.and_then(|minutes| FixedOffset::east_opt(minutes * 60))
    }

    fn local_time(&self, at: DateTime<Utc>) -> NaiveTime {
        match self.offset() {
            Some(offset) => at.with_timezone(&offset).time(),
            None => at.with_timezone(&Local).time(),
        }
    }

    fn inside(&self, time: NaiveTime) -> bool {
        match self.start.cmp(&self.end) {
            std::cmp::Ordering::Less => time >= self.start && time < self.end,
            std::cmp::Ordering::Greater => time >= self.start || time < self.end,
            std::cmp::Ordering::Equal => false,
        }
    }

    /// Whether `at` falls inside the window.
    pub fn contains(&self, at: DateTime<Utc>) -> bool {
        self.inside(self.local_time(at))
    }

    /// The earliest instant at or after `at` outside the window: `at` itself when it is
    /// outside, else the window's end.
    pub fn release_at(&self, at: DateTime<Utc>) -> DateTime<Utc> {
        if !self.contains(at) {
            return at;
        }
        let local_date = match self.offset() {
            Some(offset) => at.with_timezone(&offset).date_naive(),
            None => at.with_timezone(&Local).date_naive(),
        };
        // The end is today when it is still ahead of `at`, else tomorrow.
        for days in 0..=2 {
            let Some(date) = local_date.checked_add_days(chrono::Days::new(days)) else {
                break;
            };
            let naive = date.and_time(self.end);
            let end = match self.offset() {
                Some(offset) => offset.from_local_datetime(&naive).earliest().map(|t| t.to_utc()),
                None => Local.from_local_datetime(&naive).earliest().map(|t| t.to_utc()),
            };
            if let Some(end) = end
                && end > at
            {
                return end;
            }
        }
        at + chrono::Duration::days(1)
    }
}

impl FromStr for QuietHours {
    type Err = KernelError;

    /// Parses `HH:MM-HH:MM`, in the host's time zone.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let invalid = || KernelError::Config {
            reason: format!(
                "invalid quiet hours `{text}`; write them as HH:MM-HH:MM, e.g. 22:00-07:00"
            ),
        };
        let (start, end) = text.trim().split_once('-').ok_or_else(invalid)?;
        let start = hh_mm::parse(start.trim()).ok_or_else(invalid)?;
        let end = hh_mm::parse(end.trim()).ok_or_else(invalid)?;
        Ok(Self::new(start, end))
    }
}

/// `NaiveTime` as `HH:MM` (seconds accepted on input).
mod hh_mm {
    use chrono::NaiveTime;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn parse(text: &str) -> Option<NaiveTime> {
        NaiveTime::parse_from_str(text, "%H:%M")
            .or_else(|_| NaiveTime::parse_from_str(text, "%H:%M:%S"))
            .ok()
    }

    pub(super) fn serialize<S: Serializer>(
        time: &NaiveTime,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&time.format("%H:%M").to_string())
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<NaiveTime, D::Error> {
        let text = String::deserialize(deserializer)?;
        parse(&text)
            .ok_or_else(|| serde::de::Error::custom(format!("`{text}` is not an HH:MM time")))
    }
}

/// A partition's quotas (spec 3.2 §7.9): enforced at admission, they bound each partition
/// within a queue so none starves another (§13 Q2). `None` leaves a quota unlimited.
///
/// | Quota | Enforced |
/// |---|---|
/// | `max_running` | A claimed process whose partition already runs this many waits for a slot; an interactive one preempts the youngest background run |
/// | `max_queued` | [`ProcessTable::spawn`](crate::ProcessTable::spawn) and agenda scheduling refuse a process past it |
/// | `daily_spend_cap` | A background process is admitted only while the partition's spend today plus its budget's cost cap stays within it; else it sleeps until `BudgetReplenished` |
///
/// The `kernel.wake` runs the kernel routes wakes with are never counted or held.
/// The proactive limits — open proposals and the daily proactive budget — are the
/// partition's [`ProactivityPolicy`](crate::ProactivityPolicy).
///
/// # Example
///
/// ```rust
/// use adk_kernel::Quotas;
///
/// let quotas = Quotas::new().with_max_running(3).with_max_queued(20).with_daily_spend_cap(5_000_000);
/// assert_eq!(quotas.max_running, Some(3));
/// assert_eq!(serde_json::to_value(&quotas).unwrap()["dailySpendCap"], 5_000_000);
/// assert_eq!(Quotas::default().max_running, None);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Quotas {
    /// Most processes of the partition running at once, over every queue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_running: Option<usize>,
    /// Most processes of the partition queued at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_queued: Option<usize>,
    /// Most micro-USD the partition spends per UTC day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_spend_cap: Option<u64>,
}

impl Quotas {
    /// No quota.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the most running processes.
    #[must_use]
    pub fn with_max_running(mut self, max: usize) -> Self {
        self.max_running = Some(max);
        self
    }

    /// Sets the most queued processes.
    #[must_use]
    pub fn with_max_queued(mut self, max: usize) -> Self {
        self.max_queued = Some(max);
        self
    }

    /// Sets the daily spend cap, in micro-USD.
    #[must_use]
    pub fn with_daily_spend_cap(mut self, micro_usd: u64) -> Self {
        self.daily_spend_cap = Some(micro_usd);
        self
    }
}
