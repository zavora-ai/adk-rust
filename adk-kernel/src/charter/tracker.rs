//! The KPI tracker, guardrails and job pauses (spec 3.5 §7.11, A-I14, A-I15).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};

use adk_core::audit_log::{AuditActor, AuditTrail};
use adk_core::principal::SubjectRef;
use adk_core::{
    Clock, NewRun, PolicyDecision, SystemClock, ToolPolicy, ToolPolicyRequest,
    schedule::MissedFirePolicy,
};
use adk_server::background::{WorkflowExecutor, WorkflowState};
use async_trait::async_trait;
use awp_types::{Cadence, Charter, Direction, GuardrailAction, Kpi, Measurement};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::autonomy::{AutonomyLadder, OwnerDecision};
use super::kpi::{GuardrailState, KpiPeriod, KpiSample, KpiState, KpiStore};
use super::store::CharterStore;
use super::{CharterEvent, CharterEventSink, WEBSITE_AUDIT_KIND, audit, emit, record};
use crate::error::Result;
use crate::sched::QueueClass;
use crate::wake::{Trigger, Wake};

/// The target of the tracker's maintenance run (spec 3.5 §7.11): a durable run that a
/// [`KpiTrackerExecutor`] executes as one [`KpiTracker::tick`].
pub const KPI_TRACKER_TARGET: &str = "kernel.kpi";

/// A source of KPI measurements (spec 3.5 §7.11, §7.12): the edge beacon, host logs, an
/// analytics driver, the commerce or spend ledger, the email driver. The product and the
/// edge implement them; the tracker reads each KPI's source by its name, the KPI's
/// `SourceRef`.
///
/// # Example
///
/// ```rust
/// use adk_kernel::charter::KpiSource;
/// use chrono::{DateTime, Utc};
///
/// struct Fixed(f64);
///
/// #[async_trait::async_trait]
/// impl KpiSource for Fixed {
///     fn name(&self) -> &str {
///         "commerce"
///     }
///
///     async fn read(&self, _: uuid::Uuid, _: &str, _: DateTime<Utc>, _: DateTime<Utc>) -> adk_kernel::Result<Option<f64>> {
///         Ok(Some(self.0))
///     }
/// }
/// ```
#[async_trait]
pub trait KpiSource: Send + Sync {
    /// The source's name, as KPIs name it in their `source`.
    fn name(&self) -> &str;

    /// The value of `metric` on `site` over `[from, to)`; `None` when the source has nothing
    /// for the window yet.
    ///
    /// # Errors
    ///
    /// Returns the error that kept the source from answering; the tracker records nothing
    /// for the KPI and reads it again on the next tick.
    async fn read(
        &self,
        site: Uuid,
        metric: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Option<f64>>;
}

/// The hook a guardrail breach pauses a job through (spec 3.5 §7.11, A-I15): the harness
/// stops the job's next action. [`JobPauses`] is an implementation that is also a
/// `ToolPolicy`.
#[async_trait]
pub trait JobControl: Send + Sync {
    /// Pauses `job` of `site` for `reason` before its next action. Pausing a paused job
    /// keeps it paused.
    ///
    /// # Errors
    ///
    /// Returns the error that kept the pause from taking hold.
    async fn pause(&self, site: Uuid, job: &str, reason: &str) -> Result<()>;

    /// Lifts the pause of `job` by an owner's `decision`; `false` when it was not paused.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Unauthorized`](crate::KernelError::Unauthorized) for a decision no person made.
    async fn resume(&self, site: Uuid, job: &str, decision: &OwnerDecision) -> Result<bool>;

    /// Why `job` is paused, or `None` when it is not.
    ///
    /// # Errors
    ///
    /// Returns the error that kept the answer from being read.
    async fn paused(&self, site: Uuid, job: &str) -> Result<Option<String>>;
}

/// Paused jobs, enforced on the governed path (spec 3.5 §7.11, A-I15): as a `ToolPolicy`
/// it denies every tool call of a paused job's agents until an owner resumes the job.
///
/// Add it to the job's harness policy (it composes through `StrictestPolicy` and never
/// loosens it); the next call the job's agent makes after a pause is denied, whatever its
/// standing rules say.
///
/// # Example
///
/// ```rust
/// use adk_core::{PolicyDecision, ToolPolicy, ToolPolicyRequest};
/// use adk_kernel::charter::{JobControl, JobPauses};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let pauses = JobPauses::new().with_agent("nurture", "revenue");
/// let mut call = ToolPolicyRequest::new("send_email", serde_json::json!({}));
/// call.agent_name = "revenue".into();
/// assert_eq!(pauses.evaluate(&call).await, PolicyDecision::Allow);
/// pauses.pause(uuid::Uuid::nil(), "nurture", "unsubscribes above 2%").await?;
/// assert!(matches!(pauses.evaluate(&call).await, PolicyDecision::Deny { .. }));
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default)]
pub struct JobPauses {
    agents: RwLock<BTreeMap<String, BTreeSet<String>>>,
    paused: RwLock<BTreeMap<(Uuid, String), String>>,
}

impl JobPauses {
    /// No paused jobs and no agents.
    pub fn new() -> Self {
        Self::default()
    }

    /// Names `agent` — an agent name or its `AgentId` — as one that acts for `job`.
    #[must_use]
    pub fn with_agent(self, job: impl Into<String>, agent: impl Into<String>) -> Self {
        self.agents
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .entry(job.into())
            .or_default()
            .insert(agent.into());
        self
    }

    /// The reason of the first paused job `agent` acts for.
    fn blocking(&self, agent: &str, agent_id: Option<&str>) -> Option<(String, String)> {
        let agents = self.agents.read().unwrap_or_else(|e| e.into_inner());
        let paused = self.paused.read().unwrap_or_else(|e| e.into_inner());
        paused.iter().find_map(|((_, job), reason)| {
            let acts = agents.get(job).is_some_and(|names| {
                names.contains(agent) || agent_id.is_some_and(|id| names.contains(id))
            });
            acts.then(|| (job.clone(), reason.clone()))
        })
    }
}

#[async_trait]
impl JobControl for JobPauses {
    async fn pause(&self, site: Uuid, job: &str, reason: &str) -> Result<()> {
        self.paused
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .entry((site, job.to_string()))
            .or_insert_with(|| reason.to_string());
        Ok(())
    }

    async fn resume(&self, site: Uuid, job: &str, decision: &OwnerDecision) -> Result<bool> {
        decision.check_person()?;
        Ok(self
            .paused
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(site, job.to_string()))
            .is_some())
    }

    async fn paused(&self, site: Uuid, job: &str) -> Result<Option<String>> {
        Ok(self
            .paused
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(site, job.to_string()))
            .cloned())
    }
}

#[async_trait]
impl ToolPolicy for JobPauses {
    async fn evaluate(&self, request: &ToolPolicyRequest) -> PolicyDecision {
        let agent_id = request.acting.as_ref().map(|acting| acting.agent.id.as_str().to_string());
        match self.blocking(&request.agent_name, agent_id.as_deref()) {
            Some((job, reason)) => PolicyDecision::deny(format!(
                "job {job} is paused by a guardrail breach ({reason}); an owner resumes it \
                 (A-I15)"
            )),
            None => PolicyDecision::Allow,
        }
    }

    fn policy_ref(&self) -> Option<String> {
        Some("adk-kernel.charter:job-pauses".into())
    }
}

/// Where a KPI stands (spec 3.5 §7.11, A-I14).
///
/// # Example
///
/// ```rust
/// use adk_kernel::charter::KpiPhase;
///
/// assert_eq!(serde_json::to_value(KpiPhase::Tracking).unwrap()["phase"], "tracking");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "camelCase", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum KpiPhase {
    /// No baseline yet: the KPI is measured and has no target (A-I14).
    MeasureOnly {
        /// When the measure-only period ends.
        until: DateTime<Utc>,
    },
    /// It has a baseline; the owner may give it a target.
    Tracking,
}

/// One KPI against its baseline and target after a tick (spec 3.5 §7.11).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct KpiStatus {
    /// The KPI.
    pub kpi: String,
    /// Its phase.
    pub phase: KpiPhase,
    /// Whether this tick read its source.
    pub read: bool,
    /// Its latest measurement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<f64>,
    /// Its baseline: the charter's, else the one the tracker locked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<f64>,
    /// Its target: the charter's, and only once the charter gives it a baseline (A-I14).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<f64>,
    /// The latest against the baseline, in the KPI's direction: positive is better.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vs_baseline: Option<f64>,
    /// Whether the latest meets the target, in the KPI's direction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meets_target: Option<bool>,
}

/// What one [`KpiTracker::tick`] did (spec 3.5 §7.11).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct TrackerReport {
    /// The charter version it tracked, `None` before the first commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charter_version: Option<u32>,
    /// Every KPI's status.
    pub kpis: Vec<KpiStatus>,
    /// The KPIs whose baseline it locked.
    pub locked: Vec<String>,
    /// The guardrails that began a breach.
    pub breached: Vec<String>,
    /// The guardrails whose breach ended.
    pub recovered: Vec<String>,
    /// The jobs it paused.
    pub paused: Vec<String>,
    /// The guardrails it could not measure: no KPI tracks their metric yet.
    pub unmeasured: Vec<String>,
}

/// One KPI's week for the weekly review (spec 3.5 §7.11, §7.21).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct KpiReview {
    /// The KPI.
    pub kpi: String,
    /// Its name.
    pub name: String,
    /// Its phase.
    pub phase: KpiPhase,
    /// The week's mean.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub week: Option<f64>,
    /// The previous week's mean, for the trend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_week: Option<f64>,
    /// Its baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<f64>,
    /// Its target (A-I14).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<f64>,
}

/// The KPI tracker of one site (spec 3.5 §7.11): a kernel maintenance process that, on each
/// [`tick`](Self::tick), reads every KPI of the charter in force whose cadence is due from
/// its [`KpiSource`], records the measurement, compares it with the baseline and target,
/// keeps the measure-only period, and checks the guardrails.
///
/// | Rule | What the tracker does |
/// |---|---|
/// | Measure-only (A-I14) | A KPI with no baseline is measured for `measure_only` (14 days) from the first tick that saw it and has no target; then the tracker locks a baseline, the mean of the period's samples, and the owner may set a target |
/// | Targets (A-I14) | A charter target counts only beside a charter baseline |
/// | Guardrails (A-I15) | When a guardrail's metric, the mean of the KPIs measuring it over its window, crosses its threshold, the tracker pauses each affected job through the [`JobControl`] first (`PauseJob`), then demotes it one level and raises a P2 [`CharterEvent`]; the breach acts once until the metric recovers |
///
/// Run it daily as a durable run with [`KpiTrackerExecutor`] and [`trigger`](Self::trigger),
/// or call `tick` from the product's own loop.
///
/// # Example
///
/// ```rust,ignore
/// let tracker = KpiTracker::new(site, charters, kpis)
///     .with_source(Arc::new(beacon))
///     .with_control(pauses.clone())
///     .with_ladder(ladder);
/// let report = tracker.tick().await?;
/// ```
#[derive(Clone)]
pub struct KpiTracker {
    site: Uuid,
    charters: Arc<dyn CharterStore>,
    kpis: Arc<dyn KpiStore>,
    sources: BTreeMap<String, Arc<dyn KpiSource>>,
    control: Option<Arc<dyn JobControl>>,
    ladder: Option<AutonomyLadder>,
    events: Option<Arc<dyn CharterEventSink>>,
    audit: Option<AuditTrail>,
    clock: Arc<dyn Clock>,
    measure_only: Duration,
}

impl std::fmt::Debug for KpiTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KpiTracker")
            .field("site", &self.site)
            .field("sources", &self.sources.keys().collect::<Vec<_>>())
            .field("measure_only", &self.measure_only)
            .finish_non_exhaustive()
    }
}

impl KpiTracker {
    /// The default measure-only period, 14 days (A-I14).
    pub const DEFAULT_MEASURE_ONLY: Duration = Duration::days(14);

    /// A tracker of `site`'s charter in `charters`, recording in `kpis`, with no sources, no
    /// job control, ladder, events or audit, on the system clock.
    pub fn new(site: Uuid, charters: Arc<dyn CharterStore>, kpis: Arc<dyn KpiStore>) -> Self {
        Self {
            site,
            charters,
            kpis,
            sources: BTreeMap::new(),
            control: None,
            ladder: None,
            events: None,
            audit: None,
            clock: Arc::new(SystemClock),
            measure_only: Self::DEFAULT_MEASURE_ONLY,
        }
    }

    /// Reads KPIs whose source is `source.name()` from `source`.
    #[must_use]
    pub fn with_source(mut self, source: Arc<dyn KpiSource>) -> Self {
        self.sources.insert(source.name().to_string(), source);
        self
    }

    /// Pauses breaching jobs through `control` (A-I15).
    #[must_use]
    pub fn with_control(mut self, control: Arc<dyn JobControl>) -> Self {
        self.control = Some(control);
        self
    }

    /// Demotes breaching jobs on `ladder`.
    #[must_use]
    pub fn with_ladder(mut self, ladder: AutonomyLadder) -> Self {
        self.ladder = Some(ladder);
        self
    }

    /// Emits baselines, breaches and pauses on `sink`.
    #[must_use]
    pub fn with_events(mut self, sink: Arc<dyn CharterEventSink>) -> Self {
        self.events = Some(sink);
        self
    }

    /// Audits pauses on `trail`.
    #[must_use]
    pub fn with_audit(mut self, trail: AuditTrail) -> Self {
        self.audit = Some(trail);
        self
    }

    /// Reads time from `clock`.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Sets the measure-only period.
    #[must_use]
    pub fn with_measure_only(mut self, period: Duration) -> Self {
        self.measure_only = period;
        self
    }

    /// The site.
    pub fn site(&self) -> Uuid {
        self.site
    }

    /// The standing trigger that starts the tracker's run on the cron expression `expr`
    /// (six fields, UTC; `"0 0 3 * * *"` is daily at 03:00), on the maintenance queue of
    /// app `app_name`. Register a [`CronWakeSource`](crate::CronWakeSource) for the
    /// expression beside it.
    pub fn trigger(&self, app_name: &str, expr: &str) -> Trigger {
        let run = NewRun::new(
            QueueClass::MAINTENANCE,
            app_name,
            "kernel:kpi",
            KPI_TRACKER_TARGET,
            json!({ "site": self.site }),
        );
        Trigger::new(
            format!("{KPI_TRACKER_TARGET}:{}", self.site),
            Wake::Cron { expr: expr.to_string(), missed: MissedFirePolicy::FireOnce },
            run,
        )
    }

    /// Pauses the jobs of every guardrail whose breach the store still holds, as after a
    /// restart, before the next tick (A-I15); returns the jobs paused.
    ///
    /// # Errors
    ///
    /// Returns a store or job-control error.
    pub async fn restore_pauses(&self) -> Result<Vec<String>> {
        let (Some(control), Some(current)) =
            (&self.control, self.charters.current(self.site).await?)
        else {
            return Ok(Vec::new());
        };
        let mut paused = Vec::new();
        for guardrail in &current.charter.guardrails {
            let breached = self
                .kpis
                .guardrail(self.site, &guardrail.id)
                .await?
                .is_some_and(|state| state.breached_since.is_some());
            if breached && guardrail.action == GuardrailAction::PauseJob {
                for job in &guardrail.jobs {
                    control
                        .pause(
                            self.site,
                            job,
                            &format!("the {} guardrail is breached", guardrail.id),
                        )
                        .await?;
                    paused.push(job.clone());
                }
            }
        }
        Ok(paused)
    }

    /// One pass (spec 3.5 §7.11): reads the due KPIs, locks baselines, checks guardrails.
    ///
    /// # Errors
    ///
    /// Returns a store or job-control error; a failing source is logged and read again on
    /// the next tick.
    pub async fn tick(&self) -> Result<TrackerReport> {
        let now = self.clock.now();
        let Some(current) = self.charters.current(self.site).await? else {
            return Ok(TrackerReport::default());
        };
        let charter = &current.charter;
        let mut report =
            TrackerReport { charter_version: Some(charter.version), ..TrackerReport::default() };
        for kpi in &charter.kpis {
            let status = self.track(kpi, now, &mut report).await?;
            report.kpis.push(status);
        }
        self.guardrails(charter, now, &mut report).await?;
        Ok(report)
    }

    /// Reads, records and assesses one KPI.
    async fn track(
        &self,
        kpi: &Kpi,
        now: DateTime<Utc>,
        report: &mut TrackerReport,
    ) -> Result<KpiStatus> {
        let mut state = match self.kpis.state(self.site, &kpi.id).await? {
            Some(state) => state,
            None => KpiState::new(self.site, &kpi.id, now),
        };
        let every = cadence(kpi.cadence);
        let due = state.last_read.is_none_or(|last| now - last >= every);
        let mut read = false;
        if due {
            let source = kpi.source.to_string();
            let source = source.as_str();
            match self.sources.get(source) {
                Some(reader) => {
                    let from = state.last_read.unwrap_or(now - every);
                    match reader.read(self.site, &kpi.metric.0, from, now).await {
                        Ok(Some(value)) => {
                            self.kpis
                                .record(KpiSample::new(self.site, &kpi.id, now, value, source))
                                .await?;
                            read = true;
                            state.last_read = Some(now);
                        }
                        Ok(None) => state.last_read = Some(now),
                        Err(error) => {
                            tracing::warn!(kpi = %kpi.id, source, error = %error, "a KPI source failed; the next tick reads it again");
                        }
                    }
                }
                None => {
                    tracing::debug!(kpi = %kpi.id, source, "no KPI source registered under this name");
                }
            }
        }
        // A-I14: no baseline yet, so measure until the period ends, then lock one.
        let ends = state.measuring_since + self.measure_only;
        if kpi.baseline.is_none() && state.baseline.is_none() && now >= ends {
            let samples = self.kpis.samples(self.site, &kpi.id, state.measuring_since, now).await?;
            if !samples.is_empty() {
                let mean =
                    samples.iter().map(|sample| sample.value).sum::<f64>() / samples.len() as f64;
                state.baseline = Some(Measurement {
                    value: mean,
                    window: self.measure_only.to_std().unwrap_or_default(),
                    measured_at: now,
                });
                report.locked.push(kpi.id.clone());
                tracing::info!(kpi = %kpi.id, baseline = mean, "KPI baseline locked");
                emit(
                    self.events.as_ref(),
                    CharterEvent::BaselineLocked {
                        site: self.site,
                        kpi: kpi.id.clone(),
                        value: mean,
                        at: now,
                    },
                )
                .await;
            }
        }
        self.kpis.put_state(state.clone()).await?;
        let latest = self
            .kpis
            .samples(
                self.site,
                &kpi.id,
                now - every * 4 - Duration::days(1),
                now + Duration::microseconds(1),
            )
            .await?
            .last()
            .map(|sample| sample.value);
        let baseline =
            kpi.baseline.as_ref().or(state.baseline.as_ref()).map(|measurement| measurement.value);
        // A-I14: a target counts only beside the charter's own baseline.
        let target = kpi.baseline.as_ref().and(kpi.target.as_ref()).map(|target| target.value);
        let sign = match kpi.direction {
            Direction::Down => -1.0,
            _ => 1.0,
        };
        let phase = if baseline.is_some() {
            KpiPhase::Tracking
        } else {
            KpiPhase::MeasureOnly { until: ends }
        };
        Ok(KpiStatus {
            kpi: kpi.id.clone(),
            phase,
            read,
            latest,
            baseline,
            target,
            vs_baseline: latest.zip(baseline).map(|(latest, baseline)| sign * (latest - baseline)),
            meets_target: latest
                .zip(target)
                .map(|(latest, target)| sign * (latest - target) >= 0.0),
        })
    }

    /// Checks every guardrail; a new breach pauses its jobs first, then demotes them.
    async fn guardrails(
        &self,
        charter: &Charter,
        now: DateTime<Utc>,
        report: &mut TrackerReport,
    ) -> Result<()> {
        for guardrail in &charter.guardrails {
            let measuring: Vec<&Kpi> =
                charter.kpis.iter().filter(|kpi| kpi.metric == guardrail.metric).collect();
            let window = Duration::from_std(guardrail.window).unwrap_or(Duration::days(1));
            let from = now - window.max(Duration::seconds(1));
            let mut values = Vec::new();
            for kpi in &measuring {
                let samples = self
                    .kpis
                    .samples(self.site, &kpi.id, from, now + Duration::microseconds(1))
                    .await?;
                values.extend(samples.into_iter().map(|sample| sample.value));
            }
            if values.is_empty() {
                report.unmeasured.push(guardrail.id.clone());
                continue;
            }
            let value = values.iter().sum::<f64>() / values.len() as f64;
            let breached = guardrail.is_breached_by(value);
            let mut state = self
                .kpis
                .guardrail(self.site, &guardrail.id)
                .await?
                .unwrap_or_else(|| GuardrailState::new(self.site, &guardrail.id, now));
            let began = breached && state.breached_since.is_none();
            if !breached && state.breached_since.is_some() {
                report.recovered.push(guardrail.id.clone());
                state.breached_since = None;
            }
            if began {
                let reason = format!(
                    "the {} guardrail on {} is breached: {value} against its threshold {}",
                    guardrail.id, guardrail.metric.0, guardrail.threshold
                );
                // A-I15: the pause takes hold before anything else the breach causes.
                if guardrail.action == GuardrailAction::PauseJob {
                    for job in &guardrail.jobs {
                        if let Some(control) = &self.control {
                            control.pause(self.site, job, &reason).await?;
                        }
                        report.paused.push(job.clone());
                        record(
                            self.audit.as_ref(),
                            WEBSITE_AUDIT_KIND,
                            audit::JOB_PAUSED,
                            AuditActor::subject(SubjectRef::service("kernel:kpi")),
                            json!({
                                "site": self.site,
                                "job": job,
                                "guardrail": guardrail.id,
                                "metric": guardrail.metric.0,
                                "value": value,
                                "threshold": guardrail.threshold,
                                "charterVersion": charter.version,
                            }),
                        )
                        .await;
                        emit(
                            self.events.as_ref(),
                            CharterEvent::JobPaused {
                                site: self.site,
                                job: job.clone(),
                                reason: reason.clone(),
                                at: now,
                            },
                        )
                        .await;
                    }
                }
                state.breached_since = Some(now);
                report.breached.push(guardrail.id.clone());
                tracing::warn!(guardrail = %guardrail.id, value, threshold = guardrail.threshold, "guardrail breached");
                emit(
                    self.events.as_ref(),
                    CharterEvent::GuardrailBreached {
                        site: self.site,
                        guardrail: guardrail.id.clone(),
                        metric: guardrail.metric.0.clone(),
                        value,
                        threshold: guardrail.threshold,
                        jobs: guardrail.jobs.clone(),
                        at: now,
                    },
                )
                .await;
                if let Some(ladder) = &self.ladder {
                    for job in &guardrail.jobs {
                        if let Err(error) =
                            ladder.guardrail_breach(self.site, job, &guardrail.id).await
                        {
                            tracing::warn!(job = %job, error = %error, "could not demote a job after its guardrail breach");
                        }
                    }
                }
            }
            state.last_value = Some(value);
            state.checked_at = now;
            self.kpis.put_guardrail(state).await?;
        }
        Ok(())
    }

    /// Every KPI's week starting `week_start` (a Monday) against the week before, its
    /// baseline and its target, for the weekly review (spec 3.5 §7.21).
    ///
    /// # Errors
    ///
    /// Returns a store error.
    pub async fn review(&self, week_start: NaiveDate) -> Result<Vec<KpiReview>> {
        let Some(current) = self.charters.current(self.site).await? else {
            return Ok(Vec::new());
        };
        let mut reviews = Vec::new();
        let previous = week_start - Duration::days(7);
        for kpi in &current.charter.kpis {
            let weeks = self
                .kpis
                .aggregates(self.site, &kpi.id, KpiPeriod::Week, previous, week_start)
                .await?;
            let state = self.kpis.state(self.site, &kpi.id).await?;
            let baseline = kpi
                .baseline
                .as_ref()
                .or(state.as_ref().and_then(|state| state.baseline.as_ref()))
                .map(|measurement| measurement.value);
            let phase = match (&baseline, &state) {
                (Some(_), _) => KpiPhase::Tracking,
                (None, Some(state)) => {
                    KpiPhase::MeasureOnly { until: state.measuring_since + self.measure_only }
                }
                (None, None) => {
                    KpiPhase::MeasureOnly { until: self.clock.now() + self.measure_only }
                }
            };
            reviews.push(KpiReview {
                kpi: kpi.id.clone(),
                name: kpi.name.clone(),
                phase,
                week: weeks.last().and_then(super::kpi::KpiAggregate::mean),
                previous_week: weeks
                    .first()
                    .filter(|_| weeks.len() > 1)
                    .and_then(super::kpi::KpiAggregate::mean),
                baseline,
                target: kpi.baseline.as_ref().and(kpi.target.as_ref()).map(|target| target.value),
            });
        }
        Ok(reviews)
    }
}

/// How often a KPI of `cadence` is read.
fn cadence(cadence: Cadence) -> Duration {
    match cadence {
        Cadence::Weekly => Duration::days(7),
        Cadence::Monthly => Duration::days(30),
        Cadence::Quarterly => Duration::days(91),
        _ => Duration::days(1),
    }
}

/// Runs the tracker's maintenance runs (spec 3.5 §7.11): target [`KPI_TRACKER_TARGET`],
/// input `{"site": <uuid>}`, output the [`TrackerReport`]. Register it with
/// `KernelBuilder::workflows`, and the tracker's [`KpiTracker::trigger`] with
/// `KernelBuilder::trigger`.
#[derive(Debug, Clone)]
pub struct KpiTrackerExecutor {
    trackers: BTreeMap<Uuid, KpiTracker>,
}

impl KpiTrackerExecutor {
    /// An executor of `trackers`, one per site.
    pub fn new(trackers: impl IntoIterator<Item = KpiTracker>) -> Self {
        Self { trackers: trackers.into_iter().map(|tracker| (tracker.site(), tracker)).collect() }
    }
}

#[async_trait]
impl WorkflowExecutor for KpiTrackerExecutor {
    fn has_workflow(&self, workflow_id: &str) -> bool {
        workflow_id == KPI_TRACKER_TARGET
    }

    async fn execute(
        &self,
        workflow_id: &str,
        input: WorkflowState,
        cancel_token: CancellationToken,
    ) -> std::result::Result<Value, String> {
        if workflow_id != KPI_TRACKER_TARGET {
            return Err(format!("{workflow_id} is not the KPI tracker"));
        }
        let site = input
            .get("site")
            .and_then(Value::as_str)
            .and_then(|site| site.parse::<Uuid>().ok())
            .ok_or_else(|| {
                "a `kernel.kpi` run names its site as `{\"site\": <uuid>}`".to_string()
            })?;
        let tracker = self
            .trackers
            .get(&site)
            .ok_or_else(|| format!("no KPI tracker is registered for site {site}"))?;
        tokio::select! {
            () = cancel_token.cancelled() => Err("cancelled".to_string()),
            report = tracker.tick() => {
                let report = report.map_err(|error| error.to_string())?;
                serde_json::to_value(report).map_err(|error| error.to_string())
            }
        }
    }
}
