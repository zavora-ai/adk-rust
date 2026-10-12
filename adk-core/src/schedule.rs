//! Schedules that enqueue durable runs: cron, one-shot, and fixed-interval.
//!
//! A [`ScheduleStore`] keeps each [`Schedule`] with its next fire time and a revision.
//! A scheduler on any number of replicas polls [`due`](ScheduleStore::due), plans the
//! firings with [`plan_fires`], enqueues one run per fire time, and moves the schedule
//! on with [`advance`](ScheduleStore::advance) — a compare-and-swap on the revision.
//!
//! # Firing rule
//!
//! Every fire time enqueues a run whose idempotency key is
//! [`firing_idempotency_key`]`(id, fire_time)` = `schedule:{id}:{fire_time RFC 3339}`.
//! Replicas that plan the same fire time produce the same key, and the run store's
//! idempotent enqueue turns them into one run. The advance CAS then lets one replica
//! move the schedule on; the others see `false` and drop their plan. A replica that
//! crashes between enqueue and advance leaves the schedule due, and the next replica
//! re-plans the same fire times, which enqueue nothing new.
//!
//! # Missed fires
//!
//! When the scheduler falls behind (a restart, an outage), several occurrences can be
//! due at once. [`MissedFirePolicy`] picks which of them fire:
//!
//! | Policy | Fires |
//! |--------|-------|
//! | [`Skip`](MissedFirePolicy::Skip) | The latest due occurrence, only if it is no more than `grace` late; nothing otherwise |
//! | [`FireOnce`](MissedFirePolicy::FireOnce) | The latest due occurrence, however late |
//! | [`CatchUp(n)`](MissedFirePolicy::CatchUp) | The latest `n` due occurrences, oldest first |
//!
//! # Cron
//!
//! Cron expressions use the `cron` crate's syntax with an optional leading seconds
//! field: five fields (`min hour dom month dow`) or six or seven (`sec min hour dom
//! month dow [year]`). They are evaluated in the IANA time zone of the spec
//! (`chrono-tz`), so `0 9 * * Mon-Fri` in `Europe/Berlin` fires at 09:00 local time
//! across daylight-saving changes. A local time skipped by a spring-forward change does
//! not fire; a local time repeated by a fall-back change fires once, at its first
//! instant. Evaluating cron specs needs the `cron` feature of `adk-core`; without it
//! they fail with `schedule.cron_unavailable`.
//!
//! # Backend primitives
//!
//! | Operation | SQL | Redis | MongoDB | Neo4j, Firestore |
//! |-----------|-----|-------|---------|------------------|
//! | `advance` CAS | `UPDATE ... WHERE id = $1 AND revision = $2` | Lua compare of the revision field | `updateOne` filtered on `revision` | Transaction that checks the revision |
//! | `due` | Index on `(paused, next_fire_at)` | Sorted set of next fire times | Index on `nextFireAt` | Indexed query |

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::run_store::NewRun;
use crate::{AdkError, ErrorCategory, ErrorComponent, Result};

/// Error code returned for an unknown schedule identifier.
pub const SCHEDULE_NOT_FOUND_CODE: &str = "schedule.not_found";
/// Error code returned for an invalid schedule or spec.
pub const INVALID_SCHEDULE_CODE: &str = "schedule.invalid";

/// How late an occurrence may fire under [`MissedFirePolicy::Skip`] by default.
pub const DEFAULT_MISFIRE_GRACE: Duration = Duration::from_secs(60);

/// When a schedule fires.
///
/// # Example
///
/// ```rust
/// use adk_core::schedule::ScheduleSpec;
///
/// let spec = ScheduleSpec::Cron { expr: "0 9 * * Mon-Fri".into(), timezone: "Europe/Berlin".into() };
/// assert_eq!(serde_json::to_value(&spec).unwrap()["cron"]["timezone"], "Europe/Berlin");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum ScheduleSpec {
    /// A cron expression evaluated in an IANA time zone such as `UTC` or `America/New_York`.
    Cron {
        /// The cron expression (five, six, or seven fields).
        expr: String,
        /// The IANA time zone name.
        timezone: String,
    },
    /// Once, at this instant.
    At(DateTime<Utc>),
    /// Repeatedly, this far apart, counted from the first fire time.
    Every(Duration),
}

/// Which due occurrences fire when several are due at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum MissedFirePolicy {
    /// Fire the latest occurrence only when it is on time.
    Skip,
    /// Fire the latest occurrence, however late.
    FireOnce,
    /// Fire up to this many of the latest occurrences, oldest first; at least 1.
    CatchUp(u32),
}

/// A schedule and the run template it enqueues.
///
/// # Example
///
/// ```rust
/// use adk_core::run_store::NewRun;
/// use adk_core::schedule::{MissedFirePolicy, Schedule, ScheduleSpec};
/// use chrono::{TimeZone, Utc};
///
/// let at = Utc.with_ymd_and_hms(2026, 10, 15, 9, 0, 0).unwrap();
/// let schedule = Schedule::new(
///     "nightly-report",
///     ScheduleSpec::At(at),
///     NewRun::new("default", "reports", "system", "report_agent", serde_json::json!({})),
///     MissedFirePolicy::FireOnce,
/// )
/// .with_next_fire_at(at);
/// let run = schedule.run_for(at);
/// assert_eq!(run.idempotency_key.as_deref(), Some("schedule:nightly-report:2026-10-15T09:00:00Z"));
/// assert_eq!(
///     run.principal.unwrap().on_behalf_of,
///     Some(adk_core::SubjectRef::service("system"))
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    /// The schedule identifier.
    pub id: String,
    /// When the schedule fires.
    pub spec: ScheduleSpec,
    /// The run each firing enqueues; its idempotency key is replaced per firing.
    pub run: NewRun,
    /// Which due occurrences fire when several are due at once.
    pub missed: MissedFirePolicy,
    /// A paused schedule is never due.
    pub paused: bool,
    /// The next occurrence to fire; `None` when the schedule has no more occurrences.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_fire_at: Option<DateTime<Utc>>,
    /// Set by the store; increases by one on every change.
    pub revision: u64,
}

impl Schedule {
    /// Creates an unpaused schedule with no next fire time and revision 0.
    pub fn new(
        id: impl Into<String>,
        spec: ScheduleSpec,
        run: NewRun,
        missed: MissedFirePolicy,
    ) -> Self {
        Self { id: id.into(), spec, run, missed, paused: false, next_fire_at: None, revision: 0 }
    }

    /// Sets the next fire time.
    pub fn with_next_fire_at(mut self, at: DateTime<Utc>) -> Self {
        self.next_fire_at = Some(at);
        self
    }

    /// Sets whether the schedule is paused.
    pub fn with_paused(mut self, paused: bool) -> Self {
        self.paused = paused;
        self
    }

    /// Returns the run to enqueue for `fire_time`, keyed by [`firing_idempotency_key`].
    ///
    /// The run acts for its user as a service ([`NewRun::with_service_subject`]).
    pub fn run_for(&self, fire_time: DateTime<Utc>) -> NewRun {
        let mut run = self.run.clone().with_service_subject();
        run.idempotency_key = Some(firing_idempotency_key(&self.id, fire_time));
        run
    }

    /// Checks the fields every store requires: a non-empty id, a valid run template, a
    /// positive interval, and a positive catch-up count. Cron syntax is checked by
    /// [`ScheduleSpec::next_after`].
    ///
    /// # Errors
    ///
    /// Returns [`INVALID_SCHEDULE_CODE`], or the run template's validation error.
    pub fn validate(&self) -> Result<()> {
        if self.id.is_empty() {
            return Err(invalid_schedule_error("id must not be empty"));
        }
        if matches!(self.spec, ScheduleSpec::Every(interval) if interval.is_zero()) {
            return Err(invalid_schedule_error("an Every interval must be greater than zero"));
        }
        if self.missed == MissedFirePolicy::CatchUp(0) {
            return Err(invalid_schedule_error("CatchUp must fire at least one occurrence"));
        }
        self.run.validate()
    }

    fn same_definition(&self, other: &Self) -> bool {
        self.spec == other.spec
            && self.run == other.run
            && self.missed == other.missed
            && self.paused == other.paused
            && self.next_fire_at == other.next_fire_at
    }
}

/// Returns the idempotency key of the run a schedule enqueues for one fire time:
/// `schedule:{id}:{fire_time RFC 3339}`, in UTC with the shortest exact fraction.
///
/// # Example
///
/// ```rust
/// use chrono::{TimeZone, Utc};
///
/// let at = Utc.with_ymd_and_hms(2026, 10, 15, 9, 0, 0).unwrap();
/// assert_eq!(adk_core::schedule::firing_idempotency_key("daily", at), "schedule:daily:2026-10-15T09:00:00Z");
/// ```
pub fn firing_idempotency_key(schedule_id: &str, fire_time: DateTime<Utc>) -> String {
    format!("schedule:{schedule_id}:{}", fire_time.to_rfc3339_opts(SecondsFormat::AutoSi, true))
}

/// The fire times a scheduler enqueues for one due schedule, and where it moves next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirePlan {
    /// The occurrences to fire, oldest first.
    pub fire_times: Vec<DateTime<Utc>>,
    /// The first occurrence after `now`, for [`ScheduleStore::advance`].
    pub next_fire_at: Option<DateTime<Utc>>,
}

/// Plans the firings of a schedule that is due at `due_at`, as seen at `now`.
///
/// `due_at` (the schedule's stored `next_fire_at`) is itself an occurrence; the others
/// follow from `spec`. Occurrences in `[due_at, now]` are due, and `missed` picks which
/// fire (see the [module documentation](crate::schedule)). `grace` bounds lateness
/// under [`MissedFirePolicy::Skip`]. When `due_at` is after `now` nothing fires and
/// the schedule stays at `due_at`.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_core::schedule::{plan_fires, MissedFirePolicy, ScheduleSpec, DEFAULT_MISFIRE_GRACE};
/// use chrono::{TimeZone, Utc};
///
/// let due = Utc.with_ymd_and_hms(2026, 10, 15, 9, 0, 0).unwrap();
/// let now = due + chrono::Duration::minutes(35);
/// let plan = plan_fires(&ScheduleSpec::Every(Duration::from_secs(600)), MissedFirePolicy::CatchUp(2), due, now, DEFAULT_MISFIRE_GRACE)?;
/// assert_eq!(plan.fire_times, vec![due + chrono::Duration::minutes(20), due + chrono::Duration::minutes(30)]);
/// assert_eq!(plan.next_fire_at, Some(due + chrono::Duration::minutes(40)));
/// # Ok::<(), adk_core::AdkError>(())
/// ```
///
/// # Errors
///
/// Returns [`INVALID_SCHEDULE_CODE`] for an invalid cron expression, time zone,
/// interval, or catch-up count, and `schedule.cron_unavailable` for a cron spec when
/// the `cron` feature is off.
pub fn plan_fires(
    spec: &ScheduleSpec,
    missed: MissedFirePolicy,
    due_at: DateTime<Utc>,
    now: DateTime<Utc>,
    grace: Duration,
) -> Result<FirePlan> {
    if due_at > now {
        return Ok(FirePlan { fire_times: Vec::new(), next_fire_at: Some(due_at) });
    }
    let wanted = match missed {
        MissedFirePolicy::Skip | MissedFirePolicy::FireOnce => 1,
        MissedFirePolicy::CatchUp(0) => {
            return Err(invalid_schedule_error("CatchUp must fire at least one occurrence"));
        }
        MissedFirePolicy::CatchUp(count) => count as usize,
    };
    let mut plan = match spec {
        ScheduleSpec::At(_) => FirePlan { fire_times: vec![due_at], next_fire_at: None },
        ScheduleSpec::Every(interval) => {
            let step = chrono::Duration::from_std(*interval)
                .ok()
                .filter(|step| *step > chrono::Duration::zero())
                .ok_or_else(|| invalid_schedule_error("an Every interval must be positive"))?;
            let step_nanos = step.num_nanoseconds().unwrap_or(i64::MAX).max(1);
            let elapsed = (now - due_at).num_nanoseconds().unwrap_or(i64::MAX);
            let latest = elapsed / step_nanos;
            let first = (latest - wanted as i64 + 1).max(0);
            let at = |k: i64| {
                due_at
                    .checked_add_signed(chrono::Duration::nanoseconds(k.saturating_mul(step_nanos)))
            };
            FirePlan {
                fire_times: (first..=latest).filter_map(at).collect(),
                next_fire_at: at(latest + 1),
            }
        }
        ScheduleSpec::Cron { expr, timezone } => {
            cron::latest_and_next(expr, timezone, due_at, now, wanted)?
        }
    };
    if missed == MissedFirePolicy::Skip {
        let late = plan
            .fire_times
            .last()
            .is_some_and(|last| (now - *last).to_std().unwrap_or_default() > grace);
        if late {
            plan.fire_times.clear();
        }
    }
    Ok(plan)
}

impl ScheduleSpec {
    /// Returns the first occurrence strictly after `after`; the first fire time of a new
    /// schedule is `spec.next_after(now)` (or the instant itself for [`ScheduleSpec::At`]).
    ///
    /// For [`At`](Self::At) this is the instant when it is after `after`; for
    /// [`Every`](Self::Every) it is `after + interval`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::time::Duration;
    /// use adk_core::schedule::ScheduleSpec;
    /// use chrono::{TimeZone, Utc};
    ///
    /// let now = Utc.with_ymd_and_hms(2026, 10, 15, 9, 0, 0).unwrap();
    /// let next = ScheduleSpec::Every(Duration::from_secs(60)).next_after(now)?;
    /// assert_eq!(next, Some(Utc.with_ymd_and_hms(2026, 10, 15, 9, 1, 0).unwrap()));
    /// # Ok::<(), adk_core::AdkError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Same as [`plan_fires`].
    pub fn next_after(&self, after: DateTime<Utc>) -> Result<Option<DateTime<Utc>>> {
        match self {
            Self::At(at) => Ok((*at > after).then_some(*at)),
            Self::Every(interval) => chrono::Duration::from_std(*interval)
                .ok()
                .filter(|step| *step > chrono::Duration::zero())
                .map(|step| after.checked_add_signed(step))
                .ok_or_else(|| invalid_schedule_error("an Every interval must be positive")),
            Self::Cron { expr, timezone } => {
                Ok(cron::latest_and_next(expr, timezone, after, after, 0)?.next_fire_at)
            }
        }
    }
}

#[cfg(feature = "cron")]
mod cron {
    use std::str::FromStr;

    use chrono::{DateTime, LocalResult, TimeZone, Utc};
    use chrono_tz::Tz;

    use super::invalid_schedule_error;
    use crate::Result;

    fn parse(expr: &str, timezone: &str) -> Result<(::cron::Schedule, Tz)> {
        let expanded = if expr.split_whitespace().count() == 5 {
            format!("0 {expr}")
        } else {
            expr.to_string()
        };
        let schedule = ::cron::Schedule::from_str(&expanded).map_err(|error| {
            invalid_schedule_error(&format!("cron expression '{expr}' does not parse: {error}"))
        })?;
        let tz = timezone.parse::<Tz>().map_err(|_| {
            invalid_schedule_error(&format!(
                "time zone '{timezone}' is not an IANA name such as 'UTC' or 'Europe/Berlin'"
            ))
        })?;
        Ok((schedule, tz))
    }

    /// The later instant of a local time repeated by a fall-back change is skipped, so
    /// each local time fires once.
    fn is_repeat(at: &DateTime<Tz>) -> bool {
        matches!(at.timezone().from_local_datetime(&at.naive_local()), LocalResult::Ambiguous(first, _) if first != *at)
    }

    /// Returns up to `wanted` occurrences in `[due_at, now]` (always including `due_at`),
    /// oldest first, and the first occurrence after `now`.
    pub(super) fn latest_and_next(
        expr: &str,
        timezone: &str,
        due_at: DateTime<Utc>,
        now: DateTime<Utc>,
        wanted: usize,
    ) -> Result<super::FirePlan> {
        let (schedule, tz) = parse(expr, timezone)?;
        let next = schedule
            .after(&now.with_timezone(&tz))
            .find(|at| !is_repeat(at))
            .map(|at| at.with_timezone(&Utc));
        let mut latest = Vec::with_capacity(wanted);
        // Walks back from just past `now`, so the cost is the number of fires kept.
        let start = (now + chrono::Duration::seconds(1)).with_timezone(&tz);
        for at in schedule.after(&start).rev() {
            if latest.len() >= wanted {
                break;
            }
            let at = at.with_timezone(&Utc);
            if at <= due_at {
                break;
            }
            if at <= now && !is_repeat(&at.with_timezone(&tz)) {
                latest.push(at);
            }
        }
        if latest.len() < wanted {
            latest.push(due_at);
        }
        latest.reverse();
        Ok(super::FirePlan { fire_times: latest, next_fire_at: next })
    }
}

#[cfg(not(feature = "cron"))]
mod cron {
    use chrono::{DateTime, Utc};

    use crate::{AdkError, ErrorCategory, ErrorComponent, Result};

    pub(super) fn latest_and_next(
        _expr: &str,
        _timezone: &str,
        _due_at: DateTime<Utc>,
        _now: DateTime<Utc>,
        _wanted: usize,
    ) -> Result<super::FirePlan> {
        Err(AdkError::new(
            ErrorComponent::Agent,
            ErrorCategory::Unsupported,
            "schedule.cron_unavailable",
            "cron schedules need the `cron` feature of adk-core; enable it in Cargo.toml",
        ))
    }
}

/// Stores schedules and lets one replica at a time advance each.
///
/// Every backend passes `adk_core::schedule_store_conformance_tests!` (feature `conformance`).
#[async_trait]
pub trait ScheduleStore: Send + Sync {
    /// Inserts or replaces the schedule with `schedule.id` and returns the stored one.
    ///
    /// The store assigns the revision: 1 on insert, the previous revision plus one on a
    /// change. Upserting a definition identical to the stored one (spec, run, missed
    /// policy, paused flag, next fire time) changes nothing, so replicas registering the
    /// same schedule at startup do not disturb an advance in flight.
    ///
    /// # Errors
    ///
    /// Returns the [`Schedule::validate`] error.
    async fn upsert(&self, schedule: Schedule) -> Result<Schedule>;

    /// Returns a schedule, or `None` when it does not exist.
    async fn get(&self, id: &str) -> Result<Option<Schedule>>;

    /// Returns every schedule, ordered by id.
    async fn list(&self) -> Result<Vec<Schedule>>;

    /// Deletes a schedule.
    ///
    /// # Errors
    ///
    /// Returns [`SCHEDULE_NOT_FOUND_CODE`] for an unknown schedule.
    async fn delete(&self, id: &str) -> Result<()>;

    /// Pauses or resumes a schedule and returns it; the revision moves only on a change.
    ///
    /// # Errors
    ///
    /// Returns [`SCHEDULE_NOT_FOUND_CODE`] for an unknown schedule.
    async fn set_paused(&self, id: &str, paused: bool) -> Result<Schedule>;

    /// Returns up to `limit` unpaused schedules whose `next_fire_at` is at or before
    /// `now`, earliest first, ties by id.
    async fn due(&self, now: DateTime<Utc>, limit: usize) -> Result<Vec<Schedule>>;

    /// Sets `next_fire_at` when the stored revision equals `expected_revision`, and
    /// returns `false` when another replica advanced or changed the schedule first.
    ///
    /// # Errors
    ///
    /// Returns [`SCHEDULE_NOT_FOUND_CODE`] for an unknown schedule.
    async fn advance(
        &self,
        id: &str,
        expected_revision: u64,
        next_fire_at: Option<DateTime<Utc>>,
    ) -> Result<bool>;
}

/// Builds the error a schedule store returns for an unknown schedule.
///
/// # Example
///
/// ```rust
/// assert!(adk_core::schedule::schedule_not_found_error("daily").is_not_found());
/// ```
pub fn schedule_not_found_error(id: &str) -> AdkError {
    AdkError::not_found(
        ErrorComponent::Agent,
        SCHEDULE_NOT_FOUND_CODE,
        format!("schedule store has no schedule '{id}'; upsert it first"),
    )
}

/// Builds the error returned for an invalid schedule.
///
/// # Example
///
/// ```rust
/// let error = adk_core::schedule::invalid_schedule_error("id must not be empty");
/// assert_eq!(error.code, adk_core::schedule::INVALID_SCHEDULE_CODE);
/// ```
pub fn invalid_schedule_error(reason: &str) -> AdkError {
    AdkError::new(
        ErrorComponent::Agent,
        ErrorCategory::InvalidInput,
        INVALID_SCHEDULE_CODE,
        format!("invalid schedule: {reason}"),
    )
}

/// Process-local [`ScheduleStore`].
///
/// Holds no time-dependent state, so it takes no clock: [`due`](ScheduleStore::due)
/// receives the time from the scheduler.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_core::run_store::NewRun;
/// use adk_core::schedule::{InMemoryScheduleStore, MissedFirePolicy, Schedule, ScheduleSpec, ScheduleStore};
/// use chrono::Utc;
///
/// # futures::executor::block_on(async {
/// let store = InMemoryScheduleStore::new();
/// let now = Utc::now();
/// let schedule = Schedule::new(
///     "heartbeat",
///     ScheduleSpec::Every(Duration::from_secs(60)),
///     NewRun::new("default", "ops", "system", "probe", serde_json::json!({})),
///     MissedFirePolicy::Skip,
/// )
/// .with_next_fire_at(now);
/// let stored = store.upsert(schedule).await?;
/// assert_eq!(store.due(now, 10).await?.len(), 1);
/// assert!(store.advance("heartbeat", stored.revision, Some(now + chrono::Duration::minutes(1))).await?);
/// # Ok::<(), adk_core::AdkError>(())
/// # }).unwrap();
/// ```
#[derive(Debug, Default)]
pub struct InMemoryScheduleStore {
    schedules: Mutex<BTreeMap<String, Schedule>>,
}

impl InMemoryScheduleStore {
    /// Creates an empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ScheduleStore for InMemoryScheduleStore {
    async fn upsert(&self, mut schedule: Schedule) -> Result<Schedule> {
        schedule.validate()?;
        let mut schedules = self.schedules.lock().await;
        if let Some(existing) = schedules.get(&schedule.id) {
            if existing.same_definition(&schedule) {
                return Ok(existing.clone());
            }
            schedule.revision = existing.revision + 1;
        } else {
            schedule.revision = 1;
        }
        schedules.insert(schedule.id.clone(), schedule.clone());
        Ok(schedule)
    }

    async fn get(&self, id: &str) -> Result<Option<Schedule>> {
        Ok(self.schedules.lock().await.get(id).cloned())
    }

    async fn list(&self) -> Result<Vec<Schedule>> {
        Ok(self.schedules.lock().await.values().cloned().collect())
    }

    async fn delete(&self, id: &str) -> Result<()> {
        self.schedules
            .lock()
            .await
            .remove(id)
            .map(|_| ())
            .ok_or_else(|| schedule_not_found_error(id))
    }

    async fn set_paused(&self, id: &str, paused: bool) -> Result<Schedule> {
        let mut schedules = self.schedules.lock().await;
        let schedule = schedules.get_mut(id).ok_or_else(|| schedule_not_found_error(id))?;
        if schedule.paused != paused {
            schedule.paused = paused;
            schedule.revision += 1;
        }
        Ok(schedule.clone())
    }

    async fn due(&self, now: DateTime<Utc>, limit: usize) -> Result<Vec<Schedule>> {
        let schedules = self.schedules.lock().await;
        let mut due: Vec<_> = schedules
            .values()
            .filter(|schedule| {
                !schedule.paused && schedule.next_fire_at.is_some_and(|at| at <= now)
            })
            .cloned()
            .collect();
        due.sort_by(|a, b| (a.next_fire_at, &a.id).cmp(&(b.next_fire_at, &b.id)));
        due.truncate(limit);
        Ok(due)
    }

    async fn advance(
        &self,
        id: &str,
        expected_revision: u64,
        next_fire_at: Option<DateTime<Utc>>,
    ) -> Result<bool> {
        let mut schedules = self.schedules.lock().await;
        let schedule = schedules.get_mut(id).ok_or_else(|| schedule_not_found_error(id))?;
        if schedule.revision != expected_revision {
            return Ok(false);
        }
        schedule.next_fire_at = next_fire_at;
        schedule.revision += 1;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, day, hour, minute, 0).single().unwrap()
    }

    #[test]
    fn every_policies() {
        let spec = ScheduleSpec::Every(Duration::from_secs(600));
        let due = at(15, 9, 0);
        let now = at(15, 9, 35);
        let grace = DEFAULT_MISFIRE_GRACE;
        let plan = |missed| plan_fires(&spec, missed, due, now, grace).unwrap();
        let next = Some(at(15, 9, 40));
        assert_eq!(
            plan(MissedFirePolicy::FireOnce),
            FirePlan { fire_times: vec![at(15, 9, 30)], next_fire_at: next }
        );
        assert_eq!(
            plan(MissedFirePolicy::Skip),
            FirePlan { fire_times: vec![], next_fire_at: next }
        );
        assert_eq!(
            plan(MissedFirePolicy::CatchUp(10)),
            FirePlan {
                fire_times: vec![at(15, 9, 0), at(15, 9, 10), at(15, 9, 20), at(15, 9, 30)],
                next_fire_at: next
            }
        );
        let on_time = plan_fires(&spec, MissedFirePolicy::Skip, due, at(15, 9, 0), grace).unwrap();
        assert_eq!(on_time, FirePlan { fire_times: vec![due], next_fire_at: Some(at(15, 9, 10)) });
        let early = plan_fires(&spec, MissedFirePolicy::Skip, due, at(15, 8, 0), grace).unwrap();
        assert_eq!(early, FirePlan { fire_times: vec![], next_fire_at: Some(due) });
    }

    #[test]
    fn at_fires_once() {
        let due = at(15, 9, 0);
        let plan = plan_fires(
            &ScheduleSpec::At(due),
            MissedFirePolicy::FireOnce,
            due,
            at(15, 12, 0),
            DEFAULT_MISFIRE_GRACE,
        )
        .unwrap();
        assert_eq!(plan, FirePlan { fire_times: vec![due], next_fire_at: None });
        assert_eq!(ScheduleSpec::At(due).next_after(at(15, 8, 0)).unwrap(), Some(due));
        assert_eq!(ScheduleSpec::At(due).next_after(due).unwrap(), None);
    }

    #[test]
    fn invalid_specs_are_refused() {
        let zero = ScheduleSpec::Every(Duration::ZERO);
        assert_eq!(zero.next_after(at(15, 0, 0)).unwrap_err().code, INVALID_SCHEDULE_CODE);
        let error = plan_fires(
            &ScheduleSpec::At(at(15, 0, 0)),
            MissedFirePolicy::CatchUp(0),
            at(15, 0, 0),
            at(15, 0, 0),
            DEFAULT_MISFIRE_GRACE,
        )
        .unwrap_err();
        assert_eq!(error.code, INVALID_SCHEDULE_CODE);
    }

    #[test]
    fn firing_keys_are_canonical() {
        let fractional = at(15, 9, 0) + chrono::Duration::milliseconds(250);
        assert_eq!(
            firing_idempotency_key("a:b/ü", fractional),
            "schedule:a:b/ü:2026-10-15T09:00:00.250Z"
        );
    }

    #[cfg(feature = "cron")]
    mod cron_specs {
        use super::*;

        fn cron(expr: &str, timezone: &str) -> ScheduleSpec {
            ScheduleSpec::Cron { expr: expr.into(), timezone: timezone.into() }
        }

        #[test]
        fn five_and_six_field_expressions() {
            let five = cron("*/15 * * * *", "UTC");
            let six = cron("0 */15 * * * *", "UTC");
            assert_eq!(five.next_after(at(15, 9, 1)).unwrap(), Some(at(15, 9, 15)));
            assert_eq!(six.next_after(at(15, 9, 1)).unwrap(), Some(at(15, 9, 15)));
        }

        #[test]
        fn catch_up_and_fire_once() {
            let spec = cron("0 * * * *", "UTC");
            let due = at(15, 9, 0);
            let now = at(15, 12, 30);
            let catch_up =
                plan_fires(&spec, MissedFirePolicy::CatchUp(3), due, now, DEFAULT_MISFIRE_GRACE)
                    .unwrap();
            assert_eq!(
                catch_up,
                FirePlan {
                    fire_times: vec![at(15, 10, 0), at(15, 11, 0), at(15, 12, 0)],
                    next_fire_at: Some(at(15, 13, 0))
                }
            );
            let all =
                plan_fires(&spec, MissedFirePolicy::CatchUp(100), due, now, DEFAULT_MISFIRE_GRACE)
                    .unwrap();
            assert_eq!(
                all.fire_times,
                vec![at(15, 9, 0), at(15, 10, 0), at(15, 11, 0), at(15, 12, 0)]
            );
            let once =
                plan_fires(&spec, MissedFirePolicy::FireOnce, due, now, DEFAULT_MISFIRE_GRACE)
                    .unwrap();
            assert_eq!(once.fire_times, vec![at(15, 12, 0)]);
            let skip =
                plan_fires(&spec, MissedFirePolicy::Skip, due, now, DEFAULT_MISFIRE_GRACE).unwrap();
            assert!(skip.fire_times.is_empty());
            let on_time =
                plan_fires(&spec, MissedFirePolicy::Skip, due, at(15, 9, 0), DEFAULT_MISFIRE_GRACE)
                    .unwrap();
            assert_eq!(on_time.fire_times, vec![due]);
        }

        #[test]
        fn time_zones_and_daylight_saving() {
            // Berlin leaves summer time on 25 October 2026: 09:00 local is 07:00 UTC before, 08:00 after.
            let spec = cron("0 9 * * *", "Europe/Berlin");
            assert_eq!(spec.next_after(at(24, 0, 0)).unwrap(), Some(at(24, 7, 0)));
            assert_eq!(spec.next_after(at(25, 0, 0)).unwrap(), Some(at(25, 8, 0)));

            // 02:30 local happens twice on 25 October and fires once, at the first instant (00:30 UTC).
            let repeated = cron("0 30 2 * * *", "Europe/Berlin");
            let first = Utc.with_ymd_and_hms(2026, 10, 25, 0, 30, 0).unwrap();
            assert_eq!(repeated.next_after(at(25, 0, 0)).unwrap(), Some(first));
            assert_eq!(
                repeated.next_after(first).unwrap(),
                Some(Utc.with_ymd_and_hms(2026, 10, 26, 1, 30, 0).unwrap())
            );
            let plan = plan_fires(
                &repeated,
                MissedFirePolicy::CatchUp(5),
                first,
                at(25, 12, 0),
                DEFAULT_MISFIRE_GRACE,
            )
            .unwrap();
            assert_eq!(plan.fire_times, vec![first]);
        }

        #[test]
        fn bad_expressions_and_zones_are_refused() {
            assert_eq!(
                cron("not cron", "UTC").next_after(at(15, 0, 0)).unwrap_err().code,
                INVALID_SCHEDULE_CODE
            );
            assert_eq!(
                cron("* * * * *", "Mars/Olympus").next_after(at(15, 0, 0)).unwrap_err().code,
                INVALID_SCHEDULE_CODE
            );
        }
    }
}
