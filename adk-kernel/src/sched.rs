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

    /// Each limit the lower of `self`'s and `other`'s; a limit only one sets applies.
    pub(crate) fn stricter(&self, other: &Self) -> Self {
        fn lower<T: Ord + Copy>(a: Option<T>, b: Option<T>) -> Option<T> {
            match (a, b) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }
        }
        Self {
            max_running: lower(self.max_running, other.max_running),
            max_queued: lower(self.max_queued, other.max_queued),
            daily_spend_cap: lower(self.daily_spend_cap, other.daily_spend_cap),
        }
    }
}

/// How admission treats a claimed process (spec 3.2 §7.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ProcessClass {
    /// A human turn: never held by quiet hours or budgets; preempts background work at
    /// quota.
    Interactive,
    /// Goals, triggered and scheduled work: quotas and the partition's daily spend cap.
    Background,
    /// Agenda follow-ups and the kernel's maintenance processes: quotas, the spend cap, the
    /// daily proactive budget and quiet hours.
    Proactive,
}

/// What admission reads about a claimed process and its partition (spec 3.2 §7.9).
#[derive(Debug, Clone)]
pub(crate) struct AdmissionInput<'a> {
    pub(crate) class: ProcessClass,
    /// The run resumes after a decided approval; quiet hours never hold it.
    pub(crate) approval_woken: bool,
    pub(crate) now: DateTime<Utc>,
    pub(crate) quiet_hours: &'a [QuietHours],
    pub(crate) quotas: &'a Quotas,
    /// Admitted processes of the partition running now, this one excluded.
    pub(crate) running: usize,
    /// Whether one of them runs on the `background` queue and can be preempted.
    pub(crate) preemptible: bool,
    /// The partition's spend today, in micro-USD.
    pub(crate) partition_spent: u64,
    /// The partition's proactive spend today, in micro-USD.
    pub(crate) proactive_spent: u64,
    /// The cost cap of the daily proactive budget.
    pub(crate) daily_budget: Option<u64>,
    /// The cost cap of the process's own budget; 0 without one.
    pub(crate) need: u64,
}

/// Admission's answer for a claimed process (spec 3.2 §7.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Admission {
    /// It may run.
    Admit,
    /// Inside quiet hours: it sleeps until they end.
    QuietHours { until: DateTime<Utc> },
    /// The day's budget does not cover it: it sleeps until `BudgetReplenished`.
    Budget,
    /// The partition runs `max_running` processes: it waits for a slot, or, for an
    /// interactive one when `preempt` is set, takes the slot of the youngest background
    /// process, which parks at its next checkpoint.
    Quota { preempt: bool },
}

impl Admission {
    /// The label value a deferred process carries, `None` when admitted.
    pub(crate) fn reason(self) -> Option<&'static str> {
        match self {
            Self::Admit => None,
            Self::QuietHours { .. } => Some("quiet_hours"),
            Self::Budget => Some("budget"),
            Self::Quota { .. } => Some("quota"),
        }
    }
}

/// Whether `spent` plus `need` stays within `cap`; a process with no budget of its own
/// needs only that something is left.
fn covers(cap: Option<u64>, spent: u64, need: u64) -> bool {
    cap.is_none_or(|cap| spent.saturating_add(need.max(1)) <= cap)
}

/// Decides whether a claimed process starts now (spec 3.2 §7.9, K-I11): quiet hours first,
/// then the daily budgets, then the partition's running quota.
pub(crate) fn admit(input: &AdmissionInput<'_>) -> Admission {
    if input.class == ProcessClass::Proactive && !input.approval_woken {
        let mut until = input.now;
        // A window may end inside another; settle on an instant outside every one.
        for _ in 0..=input.quiet_hours.len() {
            let next = input
                .quiet_hours
                .iter()
                .map(|quiet| quiet.release_at(until))
                .max()
                .unwrap_or(until);
            if next == until {
                break;
            }
            until = next;
        }
        if until > input.now {
            return Admission::QuietHours { until };
        }
    }
    if input.class != ProcessClass::Interactive {
        let within_cap = covers(input.quotas.daily_spend_cap, input.partition_spent, input.need);
        let within_budget = input.class != ProcessClass::Proactive
            || covers(input.daily_budget, input.proactive_spent, input.need);
        if !within_cap || !within_budget {
            return Admission::Budget;
        }
    }
    if input.quotas.max_running.is_some_and(|max| input.running >= max) {
        return Admission::Quota {
            preempt: input.class == ProcessClass::Interactive && input.preemptible,
        };
    }
    Admission::Admit
}

/// The process an interactive one preempts (spec 3.2 §7.9): the youngest running process on
/// the `background` queue, by creation time and then id.
pub(crate) fn youngest_background<'a>(
    candidates: impl IntoIterator<Item = (&'a str, DateTime<Utc>)>,
) -> Option<&'a str> {
    candidates
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(b.0)))
        .map(|(pid, _)| pid)
}

#[cfg(test)]
mod admission_tests {
    use chrono::TimeZone;
    use proptest::prelude::*;

    use super::*;

    fn at(hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 23, hour, minute, 0).unwrap()
    }

    fn input<'a>(
        class: ProcessClass,
        quiet: &'a [QuietHours],
        quotas: &'a Quotas,
    ) -> AdmissionInput<'a> {
        AdmissionInput {
            class,
            approval_woken: false,
            now: at(12, 0),
            quiet_hours: quiet,
            quotas,
            running: 0,
            preemptible: false,
            partition_spent: 0,
            proactive_spent: 0,
            daily_budget: None,
            need: 0,
        }
    }

    fn night() -> QuietHours {
        "22:00-07:00".parse::<QuietHours>().unwrap().with_utc_offset_minutes(0)
    }

    #[test]
    fn the_admission_table() {
        let quiet = [night()];
        let quotas = Quotas::new().with_max_running(3).with_daily_spend_cap(1_000);
        let base = |class| input(class, &quiet, &quotas);

        // Free partition, daytime: everything runs.
        for class in [ProcessClass::Interactive, ProcessClass::Background, ProcessClass::Proactive]
        {
            assert_eq!(admit(&base(class)), Admission::Admit, "{class:?}");
        }
        // Quiet hours hold proactive work only, and never an approval-woken run.
        let mut late = base(ProcessClass::Proactive);
        late.now = at(23, 0);
        assert_eq!(
            admit(&late),
            Admission::QuietHours { until: Utc.with_ymd_and_hms(2026, 10, 24, 7, 0, 0).unwrap() }
        );
        late.approval_woken = true;
        assert_eq!(admit(&late), Admission::Admit);
        for class in [ProcessClass::Interactive, ProcessClass::Background] {
            let mut late = base(class);
            late.now = at(23, 0);
            assert_eq!(admit(&late), Admission::Admit, "{class:?}");
        }
        // Partition at quota: background and proactive wait; interactive preempts when it can.
        for class in [ProcessClass::Background, ProcessClass::Proactive] {
            let mut full = base(class);
            full.running = 3;
            full.preemptible = true;
            assert_eq!(admit(&full), Admission::Quota { preempt: false }, "{class:?}");
        }
        let mut full = base(ProcessClass::Interactive);
        full.running = 3;
        assert_eq!(admit(&full), Admission::Quota { preempt: false }, "nothing to preempt");
        full.preemptible = true;
        assert_eq!(admit(&full), Admission::Quota { preempt: true });
        // Spend cap: a background run whose budget the rest of the day does not cover waits.
        let mut spent = base(ProcessClass::Background);
        spent.partition_spent = 900;
        spent.need = 100;
        assert_eq!(admit(&spent), Admission::Admit, "exactly covered");
        spent.need = 101;
        assert_eq!(admit(&spent), Admission::Budget);
        spent.need = 0;
        spent.partition_spent = 1_000;
        assert_eq!(admit(&spent), Admission::Budget, "budget exhausted");
        let mut human = base(ProcessClass::Interactive);
        human.partition_spent = 5_000;
        assert_eq!(admit(&human), Admission::Admit, "a human turn is never held by spend");
        // Daily proactive budget: proactive only.
        let mut proactive = base(ProcessClass::Proactive);
        proactive.daily_budget = Some(500);
        proactive.proactive_spent = 450;
        proactive.need = 60;
        assert_eq!(admit(&proactive), Admission::Budget);
        let mut background = base(ProcessClass::Background);
        background.daily_budget = Some(500);
        background.proactive_spent = 450;
        background.need = 60;
        assert_eq!(admit(&background), Admission::Admit, "the proactive budget is not the cap");
    }

    #[test]
    fn overlapping_windows_release_outside_both() {
        let quiet = [
            "22:00-02:00".parse::<QuietHours>().unwrap().with_utc_offset_minutes(0),
            "01:00-06:30".parse::<QuietHours>().unwrap().with_utc_offset_minutes(0),
        ];
        let quotas = Quotas::new();
        let mut late = input(ProcessClass::Proactive, &quiet, &quotas);
        late.now = at(23, 0);
        assert_eq!(
            admit(&late),
            Admission::QuietHours { until: Utc.with_ymd_and_hms(2026, 10, 24, 6, 30, 0).unwrap() }
        );
    }

    #[test]
    fn preemption_takes_the_youngest_background_run() {
        let runs =
            [("run-a", at(9, 0)), ("run-c", at(11, 0)), ("run-b", at(11, 0)), ("run-d", at(8, 0))];
        assert_eq!(youngest_background(runs), Some("run-c"));
        assert_eq!(youngest_background(std::iter::empty()), None);
    }

    #[test]
    fn quiet_hours_boundaries() {
        let quiet = night();
        assert!(quiet.contains(at(22, 0)), "the start is inside");
        assert!(
            !quiet.contains(Utc.with_ymd_and_hms(2026, 10, 24, 7, 0, 0).unwrap()),
            "the end is outside"
        );
        assert!(!quiet.contains(at(21, 59)));
        let empty = "09:00-09:00".parse::<QuietHours>().unwrap().with_utc_offset_minutes(0);
        assert!(!empty.contains(at(9, 0)), "equal times make no window");
        let east = "22:00-07:00".parse::<QuietHours>().unwrap().with_utc_offset_minutes(180);
        assert!(east.contains(at(20, 0)), "23:00 at UTC+3");
        assert_eq!(
            east.release_at(at(20, 0)),
            Utc.with_ymd_and_hms(2026, 10, 24, 4, 0, 0).unwrap()
        );
        assert!("22-07".parse::<QuietHours>().is_err());
    }

    fn class() -> impl Strategy<Value = ProcessClass> {
        prop_oneof![
            Just(ProcessClass::Interactive),
            Just(ProcessClass::Background),
            Just(ProcessClass::Proactive)
        ]
    }

    #[derive(Debug, Clone)]
    enum Step {
        Claim { class: ProcessClass, need: u64 },
        Finish(usize),
        NextDay,
    }

    fn step() -> impl Strategy<Value = Step> {
        prop_oneof![
            4 => (class(), 0u64..400).prop_map(|(class, need)| Step::Claim { class, need }),
            2 => (0usize..8).prop_map(Step::Finish),
            1 => Just(Step::NextDay),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Admission never lets a partition run more than `max_running` processes, nor
        /// spend past its cap or its daily proactive budget, whatever arrives in what order;
        /// each admitted process spends at most its budget's cost cap.
        #[test]
        fn admission_never_exceeds_quota_or_budget(
            max_running in 1usize..5,
            cap in 0u64..2_000,
            budget in 0u64..1_500,
            steps in proptest::collection::vec(step(), 1..80),
        ) {
            let quotas = Quotas::new().with_max_running(max_running).with_daily_spend_cap(cap);
            let mut running: Vec<ProcessClass> = Vec::new();
            let (mut background_spent, mut proactive_spent) = (0u64, 0u64);
            for step in steps {
                match step {
                    Step::Claim { class, need } => {
                        let decision = admit(&AdmissionInput {
                            class,
                            approval_woken: false,
                            now: at(12, 0),
                            quiet_hours: &[],
                            quotas: &quotas,
                            running: running.len(),
                            preemptible: running.contains(&ProcessClass::Background),
                            partition_spent: background_spent + proactive_spent,
                            proactive_spent,
                            daily_budget: Some(budget),
                            need,
                        });
                        match decision {
                            Admission::Admit => {
                                running.push(class);
                                match class {
                                    ProcessClass::Background => background_spent += need,
                                    ProcessClass::Proactive => proactive_spent += need,
                                    ProcessClass::Interactive => {}
                                }
                            }
                            Admission::Quota { preempt: true } => {
                                // The youngest background run stops counting; the human
                                // turn takes its slot.
                                let victim = running.iter().rposition(|c| *c == ProcessClass::Background);
                                prop_assert!(victim.is_some());
                                running.remove(victim.unwrap());
                                running.push(class);
                            }
                            Admission::Quota { preempt: false } | Admission::Budget => {}
                            Admission::QuietHours { .. } => prop_assert!(false, "no quiet hours set"),
                        }
                    }
                    Step::Finish(index) => {
                        if !running.is_empty() {
                            running.remove(index % running.len());
                        }
                    }
                    Step::NextDay => {
                        background_spent = 0;
                        proactive_spent = 0;
                    }
                }
                prop_assert!(running.len() <= max_running, "{} running of {max_running}", running.len());
                prop_assert!(background_spent + proactive_spent <= cap);
                prop_assert!(proactive_spent <= budget);
            }
        }

        /// No proactive process is admitted inside quiet hours, and its release time lies
        /// outside them; interactive and approval-woken runs are never held by them.
        #[test]
        fn no_proactive_process_starts_inside_quiet_hours(
            start in 0u32..24 * 60,
            end in 0u32..24 * 60,
            offset in -12i32 * 60..14 * 60,
            minute in 0i64..3 * 24 * 60,
            class in class(),
            approval_woken in any::<bool>(),
        ) {
            let time = |m: u32| NaiveTime::from_hms_opt(m / 60, m % 60, 0).unwrap();
            let quiet = [QuietHours::new(time(start), time(end)).with_utc_offset_minutes(offset)];
            let quotas = Quotas::new();
            let now = at(0, 0) + chrono::Duration::minutes(minute);
            let mut probe = input(class, &quiet, &quotas);
            probe.now = now;
            probe.approval_woken = approval_woken;
            let decision = admit(&probe);
            let held = class == ProcessClass::Proactive && !approval_woken && quiet[0].contains(now);
            match decision {
                Admission::Admit => prop_assert!(!held),
                Admission::QuietHours { until } => {
                    prop_assert!(held);
                    prop_assert!(until > now);
                    prop_assert!(!quiet[0].contains(until));
                    prop_assert!(until - now <= chrono::Duration::days(1));
                }
                other => prop_assert!(false, "unexpected {other:?}"),
            }
        }
    }
}
