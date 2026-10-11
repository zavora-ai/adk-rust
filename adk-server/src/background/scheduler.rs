//! The durable scheduler: enqueues each schedule's fire times exactly once, from any
//! number of replicas.
//!
//! Every replica runs the same loop over [`ScheduleStore::due`]:
//!
//! 1. [`plan_fires`] picks the fire times the schedule's [`MissedFirePolicy`] keeps.
//! 2. Each fire time is enqueued with [`Schedule::run_for`], whose idempotency key is
//!    `schedule:{id}:{fire time}`, so replicas that plan the same fire time create one run.
//! 3. [`ScheduleStore::advance`] moves the schedule on by compare-and-swap on its
//!    revision; a replica that loses the race drops its plan, and its enqueues were
//!    no-ops.
//!
//! A replica that crashes between enqueue and advance leaves the schedule due; the next
//! tick re-plans the same fire times, which enqueue nothing new. While the governance
//! kill switch refuses work, nothing fires and nothing advances; once it is lifted the
//! missed-fire policy decides which of the fire times that fell due during the freeze run.
//!
//! [`MissedFirePolicy`]: adk_core::schedule::MissedFirePolicy

use std::sync::Arc;
use std::time::Duration;

use adk_core::schedule::{DEFAULT_MISFIRE_GRACE, Schedule, plan_fires};
use adk_core::{Clock, DurableRunStore, Enqueued, GovernanceControl, ScheduleStore, SystemClock};

/// What one scheduler tick did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SchedulerTick {
    /// Runs this tick created.
    pub enqueued: usize,
    /// Fire times whose run already existed (another replica enqueued it).
    pub existing: usize,
    /// Fire times refused by a schedule's concurrency key (`Skip` policy).
    pub rejected: usize,
    /// Schedules this tick advanced.
    pub advanced: usize,
    /// Schedules another replica advanced first.
    pub lost_race: usize,
    /// Whether the governance control refused the tick.
    pub frozen: bool,
}

/// Enqueues due schedules into a [`DurableRunStore`]; run one per replica.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use std::time::Duration;
/// use adk_core::{InMemoryRunStore, InMemoryScheduleStore, NewRun};
/// use adk_core::schedule::{MissedFirePolicy, Schedule, ScheduleSpec, ScheduleStore};
/// use adk_server::background::DurableScheduler;
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let schedules = Arc::new(InMemoryScheduleStore::new());
/// let runs = Arc::new(InMemoryRunStore::new());
/// let now = chrono::Utc::now();
/// schedules.upsert(
///     Schedule::new(
///         "probe",
///         ScheduleSpec::Every(Duration::from_secs(60)),
///         NewRun::new("default", "ops", "system", "probe", serde_json::json!({})),
///         MissedFirePolicy::Skip,
///     )
///     .with_next_fire_at(now),
/// ).await?;
///
/// let scheduler = DurableScheduler::new(schedules, runs);
/// assert_eq!(scheduler.tick().await?.enqueued, 1);
/// assert_eq!(scheduler.tick().await?.enqueued, 0, "advanced past the fire time");
/// # Ok::<(), adk_core::AdkError>(())
/// # }).unwrap();
/// ```
#[derive(Clone)]
pub struct DurableScheduler {
    schedules: Arc<dyn ScheduleStore>,
    runs: Arc<dyn DurableRunStore>,
    governance: Option<GovernanceControl>,
    tick_interval: Duration,
    batch: usize,
    grace: Duration,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for DurableScheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DurableScheduler")
            .field("tick_interval", &self.tick_interval)
            .field("batch", &self.batch)
            .field("grace", &self.grace)
            .finish_non_exhaustive()
    }
}

impl DurableScheduler {
    /// A scheduler that ticks every second, handles up to 100 due schedules per tick,
    /// and uses [`DEFAULT_MISFIRE_GRACE`].
    pub fn new(schedules: Arc<dyn ScheduleStore>, runs: Arc<dyn DurableRunStore>) -> Self {
        Self {
            schedules,
            runs,
            governance: None,
            tick_interval: Duration::from_secs(1),
            batch: 100,
            grace: DEFAULT_MISFIRE_GRACE,
            clock: Arc::new(SystemClock),
        }
    }

    /// Fires nothing while `control` refuses work.
    #[must_use]
    pub fn with_governance(mut self, control: GovernanceControl) -> Self {
        self.governance = Some(control);
        self
    }

    /// Sets how often [`spawn`](Self::spawn) ticks.
    #[must_use]
    pub fn with_tick_interval(mut self, interval: Duration) -> Self {
        self.tick_interval = interval;
        self
    }

    /// Sets how many due schedules one tick handles.
    #[must_use]
    pub fn with_batch(mut self, batch: usize) -> Self {
        self.batch = batch.max(1);
        self
    }

    /// Sets how late an occurrence may fire under `MissedFirePolicy::Skip`.
    #[must_use]
    pub fn with_grace(mut self, grace: Duration) -> Self {
        self.grace = grace;
        self
    }

    /// Sets the clock that decides what is due.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Enqueues the fire times of every due schedule and advances each one.
    ///
    /// # Errors
    ///
    /// Returns the schedule store's error from [`ScheduleStore::due`]. A failure on one
    /// schedule is logged and leaves it due for the next tick.
    pub async fn tick(&self) -> adk_core::Result<SchedulerTick> {
        let mut tick = SchedulerTick::default();
        if let Some(control) = &self.governance
            && let Err(error) = control.check()
        {
            tracing::debug!(error = %error, "governance refuses work; no schedule fires");
            tick.frozen = true;
            return Ok(tick);
        }
        let now = self.clock.now();
        for schedule in self.schedules.due(now, self.batch).await? {
            if let Err(error) = self.fire(&schedule, now, &mut tick).await {
                tracing::warn!(schedule.id = %schedule.id, error = %error, "could not fire schedule");
            }
        }
        Ok(tick)
    }

    async fn fire(
        &self,
        schedule: &Schedule,
        now: chrono::DateTime<chrono::Utc>,
        tick: &mut SchedulerTick,
    ) -> adk_core::Result<()> {
        let Some(due_at) = schedule.next_fire_at else { return Ok(()) };
        let plan = match plan_fires(&schedule.spec, schedule.missed, due_at, now, self.grace) {
            Ok(plan) => plan,
            Err(error) => {
                // An unplannable schedule would be due on every tick; pause it instead.
                tracing::error!(schedule.id = %schedule.id, error = %error, "schedule cannot be planned; pausing it");
                self.schedules.set_paused(&schedule.id, true).await?;
                return Ok(());
            }
        };
        // Re-checked per schedule: a freeze that lands mid-tick stops the remaining firings.
        if let Some(control) = &self.governance
            && control.check().is_err()
        {
            tick.frozen = true;
            return Ok(());
        }
        for fire_time in &plan.fire_times {
            match self.runs.enqueue(schedule.run_for(*fire_time)).await? {
                Enqueued::Created(run) => {
                    tick.enqueued += 1;
                    tracing::info!(schedule.id = %schedule.id, run.id = %run.run_id, fire_time = %fire_time, "schedule fired");
                }
                Enqueued::Existing(_) => tick.existing += 1,
                Enqueued::Rejected { blocking_run_id } => {
                    tick.rejected += 1;
                    tracing::info!(
                        schedule.id = %schedule.id,
                        fire_time = %fire_time,
                        blocking.run_id = %blocking_run_id,
                        "previous run still active; occurrence skipped"
                    );
                }
            }
        }
        if self.schedules.advance(&schedule.id, schedule.revision, plan.next_fire_at).await? {
            tick.advanced += 1;
        } else {
            tick.lost_race += 1;
        }
        Ok(())
    }

    /// Spawns the tick loop and returns its handle; abort the handle to stop it.
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(self.tick_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                if let Err(error) = self.tick().await {
                    tracing::warn!(error = %error, "scheduler tick failed");
                }
            }
        })
    }
}
