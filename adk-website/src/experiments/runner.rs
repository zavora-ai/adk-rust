//! The experiment runner (spec 3.5 §7.12, A-I15, §7.21): starts experiments, feeds each
//! day's cumulative per-arm counts to the analyzer, stops them, pauses the job on a guardrail
//! breach and writes the weekly review's evidence.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;

use adk_core::audit_log::{AuditActor, AuditTrail};
use adk_core::principal::SubjectRef;
use adk_core::schedule::MissedFirePolicy;
use adk_core::{AdkError, Clock, ErrorCategory, ErrorComponent, NewRun, Result, SystemClock};
use adk_eval::experiments::{
    Decision, ExperimentAnalyzer, ExperimentReport, Look, Sample, StopReason,
};
use adk_kernel::QueueClass;
use adk_kernel::charter::{
    AutonomyLadder, CharterEvent, CharterEventSink, CharterStore, JobControl, KpiSource, KpiStore,
    audit,
};
use adk_kernel::wake::{Trigger, Wake};
use adk_server::background::{WorkflowExecutor, WorkflowState};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::store::{ArmCounts, ExperimentRecord, ExperimentStatus, ExperimentStore};
use super::{Arm, EXPERIMENTS_WORKFLOW, ExperimentPlan, invalid};

/// Called when an experiment stops (spec 3.5 §7.12), after the job is paused and the record
/// is stored: the product publishes a release so the control is served alone at once rather
/// than at the next release.
#[async_trait]
pub trait ExperimentStopHook: Send + Sync {
    /// `record` has stopped.
    ///
    /// # Errors
    ///
    /// Returns the error that kept the release from being published; the experiment stays
    /// stopped and the next build serves the control alone.
    async fn stopped(&self, record: &ExperimentRecord) -> Result<()>;
}

/// What a look did (spec 3.5 §7.12).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LookOutcome {
    /// The experiment.
    pub experiment: String,
    /// The whole days this look added.
    pub days: u32,
    /// The record after the look, its latest report included.
    pub record: ExperimentRecord,
    /// Whether this look stopped the experiment.
    pub stopped: bool,
}

/// Runs a site's experiments (spec 3.5 §7.12, A-I15).
///
/// | Step | What it does |
/// |---|---|
/// | [`start`](Self::start) | Maps the plan onto the charter in force: the primary KPI's metric and direction, each guardrail with its tolerance from the KPI baselines; fixes the analysis; refuses while the job is paused, an id already used, or a page another running experiment holds |
/// | [`look`](Self::look) | Reads the per-arm counts of the whole days since the last look from the plan's KPI source, adds them to the totals, and analyses the totals as one cumulative `Look`, resuming the stored `ExperimentState` |
/// | Guardrail breach | Pauses the job through the kernel's `JobControl` before anything else, records `website.job.paused`, emits `CharterEvent::JobPaused` (P2) and lowers the job one level on the ladder (A-I15, §7.11) |
/// | Any final decision | Stops the experiment: the next build serves the control alone; the stop hook can publish that release at once |
/// | [`review`](Self::review) | One entry per experiment that ran in the week: its decision and the analyzer's evidence block |
///
/// Run [`look_all`](Self::look_all) once a day: register the runner with
/// `KernelBuilder::workflows` and its [`trigger`](Self::trigger) with `KernelBuilder::trigger`,
/// or call it from the product's own loop.
///
/// # Example
///
/// ```rust,ignore
/// let runner = ExperimentRunner::new(site, charters, experiments)
///     .with_source(Arc::new(BeaconKpi::new(edge.beacon())))
///     .with_kpis(kpis)
///     .with_control(kit.pauses.clone())
///     .with_ladder(ladder)
///     .with_events(alarms);
/// runner.start(plan).await?;
/// let outcomes = runner.look_all().await?;
/// ```
pub struct ExperimentRunner {
    site: Uuid,
    charters: Arc<dyn CharterStore>,
    store: Arc<dyn ExperimentStore>,
    sources: BTreeMap<String, Arc<dyn KpiSource>>,
    kpis: Option<Arc<dyn KpiStore>>,
    control: Option<Arc<dyn JobControl>>,
    ladder: Option<AutonomyLadder>,
    events: Option<Arc<dyn CharterEventSink>>,
    audit: Option<AuditTrail>,
    on_stop: Option<Arc<dyn ExperimentStopHook>>,
    clock: Arc<dyn Clock>,
    lock: Mutex<()>,
}

impl std::fmt::Debug for ExperimentRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExperimentRunner")
            .field("site", &self.site)
            .field("sources", &self.sources.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl ExperimentRunner {
    /// A runner of `site`'s experiments under its charters, keeping records in `store`, with
    /// no sources, job control, ladder, events, audit or stop hook, on the system clock.
    pub fn new(
        site: Uuid,
        charters: Arc<dyn CharterStore>,
        store: Arc<dyn ExperimentStore>,
    ) -> Self {
        Self {
            site,
            charters,
            store,
            sources: BTreeMap::new(),
            kpis: None,
            control: None,
            ladder: None,
            events: None,
            audit: None,
            on_stop: None,
            clock: Arc::new(SystemClock),
            lock: Mutex::new(()),
        }
    }

    /// Reads per-arm counts from `source` for plans that name it (`beacon` by default).
    #[must_use]
    pub fn with_source(mut self, source: Arc<dyn KpiSource>) -> Self {
        self.sources.insert(source.name().to_string(), source);
        self
    }

    /// Reads the baselines the KPI tracker locked, beside the charter's own.
    #[must_use]
    pub fn with_kpis(mut self, kpis: Arc<dyn KpiStore>) -> Self {
        self.kpis = Some(kpis);
        self
    }

    /// Pauses the job through `control` on a guardrail breach (A-I15).
    #[must_use]
    pub fn with_control(mut self, control: Arc<dyn JobControl>) -> Self {
        self.control = Some(control);
        self
    }

    /// Lowers the job one level on `ladder` on a guardrail breach (spec 3.5 §7.11).
    #[must_use]
    pub fn with_ladder(mut self, ladder: AutonomyLadder) -> Self {
        self.ladder = Some(ladder);
        self
    }

    /// Emits the pause on `sink`, such as the product's alarm router (P2).
    #[must_use]
    pub fn with_events(mut self, sink: Arc<dyn CharterEventSink>) -> Self {
        self.events = Some(sink);
        self
    }

    /// Records `website.job.paused` on `trail`.
    #[must_use]
    pub fn with_audit(mut self, trail: AuditTrail) -> Self {
        self.audit = Some(trail);
        self
    }

    /// Calls `hook` when an experiment stops.
    #[must_use]
    pub fn with_stop_hook(mut self, hook: Arc<dyn ExperimentStopHook>) -> Self {
        self.on_stop = Some(hook);
        self
    }

    /// Reads time from `clock`.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The site.
    pub fn site(&self) -> Uuid {
        self.site
    }

    /// The record store, which the [`ExperimentGenerator`](super::ExperimentGenerator) reads.
    pub fn store(&self) -> &Arc<dyn ExperimentStore> {
        &self.store
    }

    /// Starts `plan` under the charter in force (spec 3.5 §7.12).
    ///
    /// # Errors
    ///
    /// Returns `website.experiment.invalid` for an invalid plan or one the charter cannot
    /// measure (see [`ExperimentPlan::config`]), `website.experiment.exists` for an id
    /// already used on the site or a page another running experiment holds,
    /// `website.experiment.paused` while the job is paused (A-I15), and
    /// `website.experiment.no_charter` before the first charter commit.
    pub async fn start(&self, plan: ExperimentPlan) -> Result<ExperimentRecord> {
        let _guard = self.lock.lock().await;
        plan.validate()?;
        if let Some(control) = &self.control
            && let Some(reason) = control.paused(self.site, &plan.job).await.map_err(kernel)?
        {
            return Err(AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::Forbidden,
                "website.experiment.paused",
                format!(
                    "job {} is paused ({reason}); an owner resumes it before a new experiment \
                     starts (A-I15)",
                    plan.job
                ),
            ));
        }
        let current = self.charters.current(self.site).await.map_err(kernel)?.ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::NotFound,
                "website.experiment.no_charter",
                "the site has no committed charter; an experiment measures the charter's KPIs",
            )
        })?;
        for record in self.store.list(self.site).await? {
            let clash = record.plan.id == plan.id;
            let same_page =
                record.status.is_running() && record.plan.control_file()? == plan.control_file()?;
            if clash || same_page {
                return Err(AdkError::new(
                    ErrorComponent::Tool,
                    ErrorCategory::InvalidInput,
                    "website.experiment.exists",
                    if clash {
                        format!(
                            "experiment id '{}' is already used on this site; ids are never \
                             reused, so give the new experiment its own id",
                            plan.id
                        )
                    } else {
                        format!(
                            "experiment '{}' is running on {}; one experiment runs on a page at \
                             a time",
                            record.plan.id, plan.page
                        )
                    },
                ));
            }
        }
        let baselines = self.baselines(&current.charter).await?;
        let (config, guardrails) = plan.config(&current.charter, &baselines)?;
        let analyzer =
            ExperimentAnalyzer::new(config.clone()).map_err(|error| invalid(error.to_string()))?;
        let now = self.clock.now();
        let record = ExperimentRecord {
            site: self.site,
            plan,
            config,
            guardrails,
            charter_version: current.charter.version,
            status: ExperimentStatus::Running,
            started_at: now,
            counted_to: now.date_naive(),
            counts: ArmCounts::default(),
            state: analyzer.state().clone(),
            latest: None,
        };
        self.store.put(&record).await?;
        tracing::info!(site.id = %self.site, experiment = %record.plan.id, page = %record.plan.page, "experiment started");
        Ok(record)
    }

    /// Each charter metric's baseline: the charter's, else the one the tracker locked.
    async fn baselines(&self, charter: &awp_types::Charter) -> Result<BTreeMap<String, f64>> {
        let mut baselines = BTreeMap::new();
        for kpi in &charter.kpis {
            let mut value = kpi.baseline.as_ref().map(|baseline| baseline.value);
            if value.is_none()
                && let Some(kpis) = &self.kpis
            {
                value = kpis
                    .state(self.site, &kpi.id)
                    .await
                    .map_err(kernel)?
                    .and_then(|state| state.baseline.map(|baseline| baseline.value));
            }
            if let Some(value) = value {
                baselines.entry(kpi.metric.0.clone()).or_insert(value);
            }
        }
        Ok(baselines)
    }

    /// The record of experiment `id`.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    pub async fn get(&self, id: &str) -> Result<Option<ExperimentRecord>> {
        self.store.get(self.site, id).await
    }

    /// Every experiment of the site.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    pub async fn list(&self) -> Result<Vec<ExperimentRecord>> {
        self.store.list(self.site).await
    }

    /// Looks at experiment `id` (spec 3.5 §7.12): adds the whole days since the last look
    /// to its totals and analyses them; `None` when no whole day has passed since the last
    /// look. A stopped experiment returns its final record unchanged.
    ///
    /// # Errors
    ///
    /// Returns `website.experiment.unknown` for an unknown id,
    /// `website.experiment.no_source` when the plan's source is not registered, a source's
    /// or the store's error, and the analyzer's refusal of a look; a failed look changes
    /// nothing.
    pub async fn look(&self, id: &str) -> Result<Option<LookOutcome>> {
        let _guard = self.lock.lock().await;
        self.look_locked(id).await
    }

    /// Looks at every running experiment once (spec 3.5 §7.12); one experiment's failure is
    /// logged and does not stop the others.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    pub async fn look_all(&self) -> Result<Vec<LookOutcome>> {
        let _guard = self.lock.lock().await;
        let mut outcomes = Vec::new();
        for record in self.store.list(self.site).await? {
            if !record.status.is_running() {
                continue;
            }
            match self.look_locked(&record.plan.id).await {
                Ok(Some(outcome)) => outcomes.push(outcome),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(experiment = %record.plan.id, error = %error, "experiment look failed; the next look reads the same days again");
                }
            }
        }
        Ok(outcomes)
    }

    async fn look_locked(&self, id: &str) -> Result<Option<LookOutcome>> {
        let mut record = self.store.get(self.site, id).await?.ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::NotFound,
                "website.experiment.unknown",
                format!("no experiment '{id}' on this site"),
            )
        })?;
        if !record.status.is_running() {
            return Ok(Some(LookOutcome {
                experiment: id.to_string(),
                days: 0,
                record,
                stopped: false,
            }));
        }
        let now = self.clock.now();
        let today = now.date_naive();
        let days = (today - record.counted_to).num_days();
        if days <= 0 {
            return Ok(None);
        }
        let plan = record.plan.clone();
        let source = self.sources.get(&plan.source).ok_or_else(|| {
            AdkError::new(
                ErrorComponent::Tool,
                ErrorCategory::NotFound,
                "website.experiment.no_source",
                format!(
                    "no KPI source named '{}' counts experiment '{id}'; register it with \
                     `with_source`",
                    plan.source
                ),
            )
        })?;
        let from = midnight(record.counted_to);
        let to = midnight(today);
        let mut counts = record.counts.clone();
        for arm in [Arm::Control, Arm::Variant] {
            *counts.exposures.get_mut(arm) +=
                self.count(source.as_ref(), &plan.exposure_event(arm), from, to).await?;
            let metrics = std::iter::once(&record.config.primary.id)
                .chain(record.guardrails.iter().map(|guarded| &guarded.metric));
            for metric in metrics {
                *counts.events.entry(metric.clone()).or_default().get_mut(arm) +=
                    self.count(source.as_ref(), &plan.event(arm, metric), from, to).await?;
            }
        }
        let mut look = Look::new();
        for (metric, events) in &counts.events {
            // A unit is an exposed tab session; a goal reached without an exposure (another
            // tab, a script that ran late) cannot raise the rate above one.
            let sample = |arm: Arm| {
                let units = *counts.exposures.get(arm);
                Sample::proportion(units, (*events.get(arm)).min(units))
            };
            look = look.with_metric(metric.clone(), sample(Arm::Control), sample(Arm::Variant));
        }
        let mut analyzer = ExperimentAnalyzer::resume(record.config.clone(), record.state.clone())
            .map_err(|error| invalid(error.to_string()))?;
        let report = analyzer.observe(&look).map_err(|error| invalid(error.to_string()))?;
        record.counts = counts;
        record.counted_to = today;
        record.state = analyzer.state().clone();
        record.latest = Some(report.clone());
        let stopped = report.decision.is_final();
        if stopped {
            let mut paused_job = None;
            if let Decision::KeepControl { reason: StopReason::GuardrailBreach { guardrails } } =
                &report.decision
            {
                // A-I15: the job is paused before its next action, ahead of everything else.
                self.pause(&record, guardrails, &report, now).await?;
                paused_job = Some(plan.job.clone());
            }
            record.status = ExperimentStatus::Stopped {
                decision: report.decision.clone(),
                at: now,
                paused_job,
            };
        }
        self.store.put(&record).await?;
        tracing::info!(
            site.id = %self.site,
            experiment = %id,
            look = report.look,
            decision = %report.decision,
            "experiment looked at"
        );
        if stopped
            && let Some(hook) = &self.on_stop
            && let Err(error) = hook.stopped(&record).await
        {
            tracing::warn!(experiment = %id, error = %error, "the stop hook failed; the next build serves the control alone");
        }
        Ok(Some(LookOutcome {
            experiment: id.to_string(),
            days: u32::try_from(days).unwrap_or(u32::MAX),
            record,
            stopped,
        }))
    }

    /// Reads one event's count over `[from, to)`.
    async fn count(
        &self,
        source: &dyn KpiSource,
        event: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<u64> {
        let value =
            source.read(self.site, &format!("events:{event}"), from, to).await.map_err(kernel)?;
        Ok(value
            .filter(|value| value.is_finite() && *value > 0.0)
            .map_or(0, |value| value.round() as u64))
    }

    /// Pauses the job for the breached `metrics` and records why (A-I15, spec 3.5 §7.11).
    async fn pause(
        &self,
        record: &ExperimentRecord,
        metrics: &[String],
        report: &ExperimentReport,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let job = &record.plan.job;
        let breached: Vec<_> =
            record.guardrails.iter().filter(|guarded| metrics.contains(&guarded.metric)).collect();
        let names: Vec<&str> = breached.iter().map(|g| g.guardrail.as_str()).collect();
        let reason = format!(
            "experiment {} breached the {} guardrail{}: the variant made it worse beyond the \
             charter's headroom, so the control is kept",
            record.plan.id,
            names.join(", "),
            if names.len() == 1 { "" } else { "s" }
        );
        if let Some(control) = &self.control {
            control.pause(self.site, job, &reason).await.map_err(kernel)?;
        }
        for guarded in &breached {
            let value = report
                .guardrails
                .iter()
                .find(|analysis| analysis.metric == guarded.metric)
                .map(|analysis| analysis.effect.variant);
            crate::audit::record(
                self.audit.as_ref(),
                audit::JOB_PAUSED,
                AuditActor::subject(SubjectRef::service("website:experiments")),
                json!({
                    "site": self.site,
                    "job": job,
                    "guardrail": guarded.guardrail,
                    "metric": guarded.metric,
                    "value": value,
                    "threshold": guarded.threshold,
                    "charterVersion": record.charter_version,
                    "experiment": record.plan.id,
                }),
            )
            .await;
        }
        tracing::warn!(site.id = %self.site, experiment = %record.plan.id, job = %job, guardrails = ?names, "experiment guardrail breached; job paused");
        if let Some(events) = &self.events {
            events
                .emit(&CharterEvent::JobPaused {
                    site: self.site,
                    job: job.clone(),
                    reason: reason.clone(),
                    at: now,
                })
                .await;
        }
        if let Some(ladder) = &self.ladder {
            for guarded in &breached {
                if let Err(error) =
                    ladder.guardrail_breach(self.site, job, &guarded.guardrail).await
                {
                    tracing::warn!(job = %job, error = %error, "could not lower the job after the experiment's guardrail breach");
                }
            }
        }
        Ok(())
    }

    /// The weekly review's entries for `[from, to)` (spec 3.5 §7.21): every experiment that
    /// ran in the window, with its decision and the analyzer's evidence block.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    pub async fn review(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<String>> {
        let mut entries = Vec::new();
        for record in self.store.list(self.site).await? {
            let ran = record.started_at < to && record.stopped_at().is_none_or(|at| at >= from);
            if ran {
                entries.push(review_entry(&record));
            }
        }
        Ok(entries)
    }

    /// The standing trigger of the daily look on the cron expression `expr` (six fields,
    /// UTC; `"0 30 3 * * *"` is daily at 03:30), on the maintenance queue of app
    /// `app_name`.
    pub fn trigger(&self, app_name: &str, expr: &str) -> Trigger {
        let run = NewRun::new(
            QueueClass::MAINTENANCE,
            app_name,
            "website:experiments",
            EXPERIMENTS_WORKFLOW,
            json!({ "site": self.site }),
        );
        Trigger::new(
            format!("{EXPERIMENTS_WORKFLOW}:{}", self.site),
            Wake::Cron { expr: expr.to_string(), missed: MissedFirePolicy::FireOnce },
            run,
        )
    }
}

/// One experiment's weekly review entry: a line for the owner, then the evidence block.
fn review_entry(record: &ExperimentRecord) -> String {
    let plan = &record.plan;
    let mut entry = format!("Experiment `{}` on {}", plan.id, plan.page);
    if !plan.hypothesis.is_empty() {
        let _ = write!(entry, " — {}", plan.hypothesis);
    }
    let _ = write!(entry, " (started {}): ", record.started_at.date_naive());
    match &record.status {
        ExperimentStatus::Running => entry.push_str("running"),
        ExperimentStatus::Stopped { decision, at, paused_job } => {
            let _ = write!(entry, "stopped {}, {decision}", at.date_naive());
            if let Some(job) = paused_job {
                let _ = write!(
                    entry,
                    "; the {job} job is paused until you resume it, and the control is served"
                );
            } else if decision.keeps_control() {
                entry.push_str("; the control is served");
            }
        }
    }
    entry.push('.');
    match &record.latest {
        Some(report) => {
            entry.push_str("\n\n  ```text\n");
            for line in report.to_string().lines() {
                let _ = writeln!(entry, "  {line}");
            }
            entry.push_str("  ```");
        }
        None => entry.push_str(" No whole day counted yet."),
    }
    entry
}

/// Midnight UTC at the start of `day`.
fn midnight(day: NaiveDate) -> DateTime<Utc> {
    day.and_hms_opt(0, 0, 0).map_or_else(Utc::now, |time| time.and_utc())
}

fn kernel(error: adk_kernel::KernelError) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::Unavailable,
        "website.experiment.kernel",
        error.to_string(),
    )
}

#[async_trait]
impl WorkflowExecutor for ExperimentRunner {
    fn has_workflow(&self, workflow_id: &str) -> bool {
        workflow_id == EXPERIMENTS_WORKFLOW
    }

    async fn execute(
        &self,
        workflow_id: &str,
        input: WorkflowState,
        cancel_token: CancellationToken,
    ) -> std::result::Result<Value, String> {
        if workflow_id != EXPERIMENTS_WORKFLOW {
            return Err(format!("{workflow_id} is not the experiments look"));
        }
        let site = input.get("site").and_then(Value::as_str).and_then(|s| s.parse::<Uuid>().ok());
        if site != Some(self.site) {
            return Err(format!(
                "a `{EXPERIMENTS_WORKFLOW}` run names its site as {{\"site\": \"{}\"}}",
                self.site
            ));
        }
        tokio::select! {
            () = cancel_token.cancelled() => Err("cancelled".to_string()),
            outcomes = self.look_all() => {
                let outcomes = outcomes.map_err(|error| error.to_string())?;
                let looks: Vec<Value> = outcomes
                    .iter()
                    .map(|outcome| json!({
                        "experiment": outcome.experiment,
                        "days": outcome.days,
                        "stopped": outcome.stopped,
                        "decision": outcome.record.latest.as_ref().map(|report| report.decision.clone()),
                    }))
                    .collect();
                Ok(json!({ "looks": looks }))
            }
        }
    }
}
