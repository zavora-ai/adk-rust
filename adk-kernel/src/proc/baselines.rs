//! The daily baseline pass (spec 3.2 §7.10, §9.2 K-I20, §13 Q4; feature `proc-baselines`).
//!
//! Once a local day has ended, the kernel's maintenance tick enqueues one `Maintenance`
//! process, the durable run `kernel.proc` ([`PROC_TARGET`]) keyed by the day, so replicas
//! sharing a run store run it once. It calls `adk-eval`'s ledger statistics — no
//! statistics live here — for each partition and each day since its last pass:
//!
//! | Step | Rule | Where |
//! |---|---|---|
//! | Series | one value per local day and dimension, from the partition's rows; the kernel's own rows ([`KERNEL_AGENT`]) and observations are left out | `adk_eval::ledger::daily_series` |
//! | Baseline | median and MAD per dimension and day class over 28 days, after a 14-day warm-up | `adk_eval::ledger::check_series` |
//! | Deviation | `\|recent − baseline\| > max(k·MAD, floor)`, k = 2.5, in both the 7- and the 14-day window | `adk_eval::ledger::check_series` |
//! | Meaningful | two dimensions, or one for ten days | `adk_eval::ledger::DriftState::advance` |
//! | Cooldown | seven days per dimension after an observation names it | `adk_eval::ledger::DriftState::advance` |
//! | Wording | the model phrases what was detected, within the daily budget; a reply that is not a neutral question falls back to the kernel's template | [`ObservationPhraser`], [`is_neutral_question`], [`template_question`] |
//!
//! The model sees only the dimension names, their directions, run lengths and the
//! comparison windows — never a ledger row — and is used only after the deterministic
//! detection found something (K-I20). Each observation is stored, published as
//! `KernelEvent::Observation` and audited as `kernel.observation`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind, AuditTrail};
use adk_core::{
    Content, DurableRunStore, Enqueued, GenerateContentConfig, Llm, LlmRequest, NewRun, RunBudget,
    SubjectRef,
};
use adk_eval::ledger::{
    DeviationConfig, DeviationReport, DimensionDeviation, DriftState, DriftStep, LedgerRow,
    RECENT_WINDOW_DAYS, check_series, daily_series,
};
use async_trait::async_trait;
use chrono::{DateTime, Days, NaiveDate, Utc};
use futures::StreamExt as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    Direction, KERNEL_AGENT, Ledger, LedgerEntry, LedgerKind, LedgerQuery, Observation,
    ObservedDimension, PassState, Phrasing, is_neutral_question, template_question,
};
use crate::agenda::AgendaHandle;
use crate::bus::{EventBus, KernelEvent};
use crate::error::Result;
use crate::partition::PartitionId;
use crate::sched::QueueClass;

/// The workflow target of the daily pass's `Maintenance` process (spec 3.2 §7.10).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_kernel::proc::baselines::PROC_TARGET, "kernel.proc");
/// ```
pub const PROC_TARGET: &str = "kernel.proc";

/// Days of rows one day's test reads: the 28-day baseline window and the 14-day comparison
/// window before it.
const TEST_SPAN_DAYS: u64 = 28 + 14;

/// Rows one read of the ledger fetches.
const PAGE: usize = 5_000;

/// How the kernel runs the daily baseline pass (spec 3.2 §7.10).
///
/// | Field | Default | Meaning |
/// |---|---|---|
/// | `enabled` | `true` | Whether the maintenance tick runs the pass |
/// | `deviation` | k = 2.5, a 45-minute floor for the time-of-day dimensions | The deviation test's settings (`adk_eval::ledger::DeviationConfig`) |
/// | `daily_budget` | 3 model calls, 2 000 tokens | What one day's pass may spend phrasing; past it the template words the rest |
/// | `max_catch_up_days` | 42 | Most days a pass tests after the kernel was down |
///
/// # Example
///
/// ```rust
/// use adk_kernel::proc::baselines::BaselinePolicy;
///
/// let policy = BaselinePolicy::default().with_floor("tool_call.count", 3.0);
/// assert!(policy.enabled);
/// assert_eq!(policy.daily_budget.max_model_calls, Some(3));
/// assert_eq!(policy.deviation.floor("tool_call.count"), 3.0);
/// ```
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct BaselinePolicy {
    /// Whether the maintenance tick runs the pass.
    pub enabled: bool,
    /// The deviation test's k and floors.
    pub deviation: DeviationConfig,
    /// What one day's pass may spend phrasing observations.
    pub daily_budget: RunBudget,
    /// Most days one pass tests, counting back from the day that ended.
    pub max_catch_up_days: u32,
}

impl Default for BaselinePolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            deviation: DeviationConfig::default(),
            daily_budget: RunBudget::new().max_model_calls(3).max_total_tokens(2_000),
            max_catch_up_days: 42,
        }
    }
}

impl BaselinePolicy {
    /// A policy that runs no pass.
    pub fn disabled() -> Self {
        Self { enabled: false, ..Self::default() }
    }

    /// Sets the floor of one dimension.
    #[must_use]
    pub fn with_floor(mut self, dimension: impl Into<String>, floor: f64) -> Self {
        self.deviation = self.deviation.with_floor(dimension, floor);
        self
    }

    /// Sets the deviation test's settings.
    #[must_use]
    pub fn with_deviation(mut self, deviation: DeviationConfig) -> Self {
        self.deviation = deviation;
        self
    }

    /// Sets what one day's pass may spend phrasing.
    #[must_use]
    pub fn with_daily_budget(mut self, budget: RunBudget) -> Self {
        self.daily_budget = budget;
        self
    }

    /// Sets the most days one pass tests.
    #[must_use]
    pub fn with_max_catch_up_days(mut self, days: u32) -> Self {
        self.max_catch_up_days = days.max(1);
        self
    }
}

/// What the deterministic detection found, as the phraser sees it (spec 3.2 §7.10): the
/// dimensions, which way they moved and for how long, and the windows — no value, no row.
///
/// # Example
///
/// ```rust
/// use adk_kernel::proc::baselines::Detection;
/// use adk_kernel::proc::{Direction, ObservedDimension};
/// use chrono::NaiveDate;
///
/// let day = NaiveDate::from_ymd_opt(2026, 10, 22).unwrap();
/// let detection = Detection::new("default", day, vec![ObservedDimension::new("activity.last_minute", Direction::Higher, day, 10)], vec![7, 14]);
/// assert!(detection.prompt().contains("activity.last_minute"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Detection {
    /// The partition.
    pub partition: PartitionId,
    /// The day the test ran on.
    pub as_of: NaiveDate,
    /// The dimensions that changed.
    pub dimensions: Vec<ObservedDimension>,
    /// The comparison windows, in days.
    pub windows: Vec<u32>,
}

impl Detection {
    /// A detection of `dimensions` in `partition` on `as_of`.
    pub fn new(
        partition: impl Into<PartitionId>,
        as_of: NaiveDate,
        dimensions: Vec<ObservedDimension>,
        windows: Vec<u32>,
    ) -> Self {
        Self { partition: partition.into(), as_of, dimensions, windows }
    }

    /// The question the kernel's template words for it.
    pub fn template(&self) -> String {
        template_question(&self.dimensions, &self.windows)
    }

    /// The detection as the phraser's user message: the template's wording and the
    /// dimensions as JSON.
    pub fn prompt(&self) -> String {
        let dimensions: Vec<Value> = self
            .dimensions
            .iter()
            .map(|dimension| {
                json!({
                    "dimension": dimension.dimension,
                    "direction": dimension.direction,
                    "days": dimension.days,
                })
            })
            .collect();
        format!(
            "Detected over the comparison windows of {:?} days: {}\nA plain wording: {}",
            self.windows,
            Value::Array(dimensions),
            self.template()
        )
    }
}

/// The instruction a model phrases an observation under (spec 3.2 §7.10).
pub const PHRASING_INSTRUCTION: &str = "You word one observation about a person's activity \
pattern for them to read. The detection below was made by a deterministic statistical test; \
do not add to it, explain it or guess its cause. Write one or two short sentences in neutral, \
friendly language that say what changed and end with one question offering to review it, \
such as \"Would you like to review what changed?\". Never diagnose, judge, warn or advise: \
no words about health, stress, tiredness, problems or what the person should do. Answer \
with the text only, on one line.";

/// Words a detection as a question (spec 3.2 §7.10): a model, used only after the
/// deterministic detection found something. The kernel keeps a reply only when it is a
/// neutral question ([`is_neutral_question`]) and uses its template otherwise.
///
/// # Example
///
/// ```rust
/// use adk_core::RunBudget;
/// use adk_kernel::proc::baselines::{Detection, ObservationPhraser};
/// use async_trait::async_trait;
///
/// struct Template;
///
/// #[async_trait]
/// impl ObservationPhraser for Template {
///     fn name(&self) -> &str {
///         "template"
///     }
///
///     async fn phrase(&self, detection: &Detection, _budget: &RunBudget) -> adk_kernel::Result<String> {
///         Ok(detection.template())
///     }
/// }
/// ```
#[async_trait]
pub trait ObservationPhraser: Send + Sync {
    /// The model's name, recorded as the observation's [`Phrasing::Model`].
    fn name(&self) -> &str;

    /// The question for `detection`, within `budget`.
    ///
    /// # Errors
    ///
    /// Returns the model's error; the kernel then words the observation itself.
    async fn phrase(&self, detection: &Detection, budget: &RunBudget) -> Result<String>;
}

/// An [`ObservationPhraser`] over any `adk_core::Llm` (spec 3.2 §7.10): one request with
/// [`PHRASING_INSTRUCTION`] as its system message and the [`Detection::prompt`] as its
/// user message, its output capped by the budget's token limit (at most 1 024) and no
/// sampling parameters.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_kernel::proc::baselines::{LlmPhraser, ObservationPhraser};
/// use adk_model::{ScriptedLlm, ScriptedResponse};
///
/// let model = Arc::new(ScriptedLlm::new("scripted", vec![ScriptedResponse::text("…")]));
/// assert_eq!(LlmPhraser::new(model).name(), "scripted");
/// ```
pub struct LlmPhraser {
    model: Arc<dyn Llm>,
}

impl std::fmt::Debug for LlmPhraser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmPhraser").field("model", &self.model.name()).finish()
    }
}

impl LlmPhraser {
    /// A phraser over `model`.
    pub fn new(model: Arc<dyn Llm>) -> Self {
        Self { model }
    }
}

#[async_trait]
impl ObservationPhraser for LlmPhraser {
    fn name(&self) -> &str {
        self.model.name()
    }

    async fn phrase(&self, detection: &Detection, budget: &RunBudget) -> Result<String> {
        // No sampling parameters: models with adaptive thinking reject them.
        let max_output_tokens = budget
            .max_total_tokens
            .map(|tokens| i32::try_from(tokens.min(1_024)).unwrap_or(1_024))
            .unwrap_or(1_024);
        let request = LlmRequest::new(
            self.model.name(),
            vec![
                Content::new("system").with_text(PHRASING_INSTRUCTION),
                Content::new("user").with_text(detection.prompt()),
            ],
        )
        .with_config(GenerateContentConfig {
            max_output_tokens: Some(max_output_tokens),
            ..GenerateContentConfig::default()
        });
        let mut responses = self.model.generate_content(request, false).await?;
        let mut whole = String::new();
        let mut last = None;
        while let Some(response) = responses.next().await {
            let response = response?;
            let text: String = response
                .content
                .iter()
                .flat_map(|content| content.parts.iter())
                .filter_map(|part| part.text())
                .collect();
            whole.push_str(&text);
            if !response.partial {
                last = Some(text);
            }
        }
        Ok(last.filter(|text| !text.trim().is_empty()).unwrap_or(whole).trim().to_string())
    }
}

/// A partition's baselines on the day its last pass tested (spec 3.2 §7.10): the deviation
/// test of every dimension, with its baseline per day class, warm-up and windows. Stored as
/// [`PassState::baselines`] and served at `GET /api/kernel/proc/baselines`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct BaselineSnapshot {
    /// The partition.
    pub partition: PartitionId,
    /// The day tested.
    pub as_of: NaiveDate,
    /// Every dimension's test.
    pub dimensions: Vec<DimensionDeviation>,
}

/// What one pass did (spec 3.2 §7.10).
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct PassOutcome {
    /// The observations it recorded, oldest first.
    pub observations: Vec<Observation>,
    /// The partitions it tested.
    pub partitions: Vec<PartitionId>,
    /// The days it tested, per partition.
    pub days: usize,
    /// The model calls it made phrasing.
    pub model_calls: u64,
}

/// The daily baseline pass over a [`Ledger`] (spec 3.2 §7.10): what the kernel's
/// `kernel.proc` process runs, and what a test or a tool runs directly.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_kernel::proc::baselines::{BaselinePass, BaselinePolicy};
/// use adk_kernel::proc::{InMemoryLedgerStore, Ledger, LedgerKey};
/// use chrono::NaiveDate;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let ledger = Ledger::new(Arc::new(InMemoryLedgerStore::new()), LedgerKey::from_bytes([1; 32]));
/// let outcome = BaselinePass::new(ledger, BaselinePolicy::default())
///     .run(NaiveDate::from_ymd_opt(2026, 10, 22).unwrap())
///     .await?;
/// assert!(outcome.observations.is_empty(), "an empty ledger has no baseline");
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct BaselinePass {
    ledger: Ledger,
    policy: BaselinePolicy,
    phraser: Option<Arc<dyn ObservationPhraser>>,
}

impl std::fmt::Debug for BaselinePass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BaselinePass")
            .field("policy", &self.policy)
            .field("phraser", &self.phraser.as_ref().map(|phraser| phraser.name().to_string()))
            .finish_non_exhaustive()
    }
}

impl BaselinePass {
    /// A pass over `ledger` under `policy`, worded by the template.
    pub fn new(ledger: Ledger, policy: BaselinePolicy) -> Self {
        Self { ledger, policy, phraser: None }
    }

    /// Words observations with `phraser`, within the policy's daily budget.
    #[must_use]
    pub fn with_phraser(mut self, phraser: Arc<dyn ObservationPhraser>) -> Self {
        self.phraser = Some(phraser);
        self
    }

    /// The ledger the pass reads.
    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    /// The policy it runs under.
    pub fn policy(&self) -> &BaselinePolicy {
        &self.policy
    }

    /// Tests every day from each partition's last pass through `as_of` (at most
    /// `max_catch_up_days`), records the observations the drift state yields, and stores
    /// each partition's new pass state. A day already tested is not tested again, so a
    /// repeated pass records nothing new.
    ///
    /// # Errors
    ///
    /// Returns the ledger store's error. A phrasing failure is not an error: the template
    /// words that observation.
    pub async fn run(&self, as_of: NaiveDate) -> Result<PassOutcome> {
        let store = self.ledger.store();
        let mut states: BTreeMap<PartitionId, PassState> = store
            .pass_states()
            .await?
            .into_iter()
            .map(|state| (state.partition.clone(), state))
            .collect();
        let floor = as_of
            .checked_sub_days(Days::new(u64::from(self.policy.max_catch_up_days) - 1))
            .unwrap_or(as_of);
        let start = |state: Option<&PassState>| {
            state.and_then(|state| state.last_day.succ_opt()).map_or(floor, |next| next.max(floor))
        };
        let first_day =
            states.values().map(|state| start(Some(state))).min().unwrap_or(floor).min(floor);
        let rows = self.rows(first_day, as_of).await?;
        let partitions: BTreeSet<PartitionId> =
            rows.keys().cloned().chain(states.keys().cloned()).collect();

        let mut outcome = PassOutcome::default();
        let mut detections: Vec<Detection> = Vec::new();
        let mut updated = Vec::new();
        for partition in partitions {
            let previous = states.remove(&partition);
            let from = start(previous.as_ref());
            if from > as_of {
                continue;
            }
            let series = daily_series(rows.get(&partition).map_or(&[][..], Vec::as_slice));
            let mut drift: DriftState = previous
                .as_ref()
                .and_then(|state| serde_json::from_value(state.drift.clone()).ok())
                .unwrap_or_default();
            let mut report = check_series(&series, from, &self.policy.deviation);
            for day in from.iter_days().take_while(|day| *day <= as_of) {
                report = check_series(&series, day, &self.policy.deviation);
                let step = drift.advance(day, &report.deviating());
                if !step.observed.is_empty() {
                    detections.push(detection(&partition, day, &step, &report));
                }
                drift = step.state;
                outcome.days += 1;
            }
            let snapshot = BaselineSnapshot {
                partition: partition.clone(),
                as_of,
                dimensions: report.dimensions,
            };
            let mut state = PassState::new(partition.clone(), as_of, self.ledger.clock().now());
            state.drift = serde_json::to_value(&drift)?;
            state.baselines = serde_json::to_value(&snapshot)?;
            updated.push(state);
            outcome.partitions.push(partition);
        }

        let calls = self.policy.daily_budget.max_model_calls.unwrap_or(u64::MAX);
        for detection in detections {
            let (question, phrasing) = match &self.phraser {
                Some(phraser) if outcome.model_calls < calls => {
                    outcome.model_calls += 1;
                    match phraser.phrase(&detection, &self.policy.daily_budget).await {
                        Ok(text) if is_neutral_question(&text) => {
                            (text, Phrasing::Model { model: phraser.name().to_string() })
                        }
                        Ok(_) => {
                            tracing::warn!(
                                observation.partition = %detection.partition,
                                "the model's wording is not a neutral question; using the template"
                            );
                            (detection.template(), Phrasing::Template)
                        }
                        Err(error) => {
                            tracing::warn!(error = %error, "the model could not word an observation; using the template");
                            (detection.template(), Phrasing::Template)
                        }
                    }
                }
                _ => (detection.template(), Phrasing::Template),
            };
            let observation = Observation {
                id: Observation::id_for(&detection.partition, detection.as_of),
                partition: detection.partition,
                as_of: detection.as_of,
                dimensions: detection.dimensions,
                windows: detection.windows,
                question,
                phrasing,
                created_at: self.ledger.clock().now(),
            };
            store.put_observation(observation.clone()).await?;
            outcome.observations.push(observation);
        }
        // Stored after the observations, so a pass cut short repeats its days.
        for state in updated {
            store.put_pass_state(state).await?;
        }
        Ok(outcome)
    }

    /// The activity rows that feed the tests of `first_day` through `as_of`, by partition,
    /// in `adk-eval`'s row form: the kernel's own work and observations left out.
    async fn rows(
        &self,
        first_day: NaiveDate,
        as_of: NaiveDate,
    ) -> Result<BTreeMap<PartitionId, Vec<LedgerRow>>> {
        let span_start = first_day.checked_sub_days(Days::new(TEST_SPAN_DAYS)).unwrap_or(first_day);
        // A day either side, so every row whose local day is in the span is read whatever
        // its offset.
        let from = local_midnight(span_start.pred_opt().unwrap_or(span_start));
        let to = local_midnight(as_of.succ_opt().and_then(|day| day.succ_opt()).unwrap_or(as_of));
        let mut rows: BTreeMap<PartitionId, Vec<LedgerRow>> = BTreeMap::new();
        let mut query = LedgerQuery::default().from(from).to(to).limit(PAGE);
        loop {
            let page = self.ledger.store().list(&query).await?;
            let Some(last) = page.last().map(|entry| entry.seq) else { break };
            for entry in page.iter().filter(|entry| is_activity(entry)) {
                rows.entry(entry.partition.clone()).or_default().push(row(entry));
            }
            if page.len() < PAGE {
                break;
            }
            query = query.after(last);
        }
        Ok(rows)
    }
}

/// UTC midnight of `day`; the read window has a day's margin either side, so the offset
/// of the rows does not matter here.
fn local_midnight(day: NaiveDate) -> DateTime<Utc> {
    day.and_time(chrono::NaiveTime::MIN).and_utc()
}

/// Whether a row is the user's activity: not the kernel's own work, not an observation.
fn is_activity(entry: &LedgerEntry) -> bool {
    entry.agent != KERNEL_AGENT && entry.kind != LedgerKind::Observation.as_str()
}

/// A kernel row in `adk-eval`'s form.
fn row(entry: &LedgerEntry) -> LedgerRow {
    LedgerRow {
        ts: entry.ts,
        partition: entry.partition.as_str().to_string(),
        agent: entry.agent.clone(),
        kind: entry.kind.clone(),
        effect: entry.effect.clone(),
        duration_ms: entry.duration_ms,
        subject_hash: entry.subject_hash.clone(),
        meta: entry.meta.clone(),
        trace_id: entry.trace_id.clone(),
    }
}

/// The detection of `step`'s observed dimensions on `day`, with the direction each moved
/// in the class that deviated, read over the longest window.
fn detection(
    partition: &PartitionId,
    day: NaiveDate,
    step: &DriftStep,
    report: &DeviationReport,
) -> Detection {
    let dimensions = step
        .observed
        .iter()
        .map(|name| {
            let run = step.runs.iter().find(|run| &run.dimension == name);
            let direction = report
                .dimensions
                .iter()
                .find(|test| &test.dimension == name)
                .and_then(|test| test.classes.iter().find(|class| class.deviating))
                .and_then(|class| {
                    let baseline = class.baseline?;
                    let recent =
                        class.windows.iter().max_by_key(|window| window.days)?.recent_median?;
                    Some(if recent >= baseline.median {
                        Direction::Higher
                    } else {
                        Direction::Lower
                    })
                })
                .unwrap_or(Direction::Higher);
            ObservedDimension::new(
                name.clone(),
                direction,
                run.map_or(day, |run| run.since),
                run.map_or(1, |run| run.days),
            )
        })
        .collect();
    Detection::new(partition.clone(), day, dimensions, RECENT_WINDOW_DAYS.to_vec())
}

/// The kernel's side of the pass: the tick that enqueues it, and the `kernel.proc`
/// workflow that runs it and publishes what it found.
pub(crate) struct ProcRuntime {
    pub(crate) pass: BaselinePass,
    pub(crate) bus: EventBus,
    pub(crate) audit: Option<AuditTrail>,
    pub(crate) kernel_id: String,
    pub(crate) app_name: String,
    /// Where observations are filed as questions.
    agenda: AgendaHandle,
    /// The last day this replica enqueued a pass for.
    enqueued: Mutex<Option<NaiveDate>>,
}

impl ProcRuntime {
    pub(crate) fn new(
        pass: BaselinePass,
        bus: EventBus,
        audit: Option<AuditTrail>,
        kernel_id: String,
        app_name: String,
        agenda: AgendaHandle,
    ) -> Self {
        Self { pass, bus, audit, kernel_id, app_name, agenda, enqueued: Mutex::new(None) }
    }

    /// On the maintenance tick: enqueues the pass of the local day that ended before `now`,
    /// once per day; `true` when it created the run. The key `kernel:proc:<day>` makes one
    /// run per day across replicas.
    pub(crate) async fn tick(
        &self,
        runs: &Arc<dyn DurableRunStore>,
        queues: &[QueueClass],
        now: DateTime<Utc>,
    ) -> bool {
        if !self.pass.policy.enabled
            || !queues.iter().any(|queue| queue.as_str() == QueueClass::MAINTENANCE)
        {
            return false;
        }
        let Some(as_of) = self.pass.ledger.offset().date_at(now).pred_opt() else {
            return false;
        };
        if self.enqueued.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|day| day >= as_of) {
            return false;
        }
        let run = NewRun::new(
            QueueClass::MAINTENANCE,
            self.app_name.clone(),
            KERNEL_AGENT,
            PROC_TARGET,
            json!({ "asOf": as_of }),
        )
        .with_idempotency_key(format!("kernel:proc:{as_of}"))
        .with_service_subject();
        match runs.enqueue(run).await {
            Ok(enqueued) => {
                *self.enqueued.lock().unwrap_or_else(|e| e.into_inner()) = Some(as_of);
                matches!(enqueued, Enqueued::Created(_))
            }
            Err(error) => {
                tracing::warn!(error = %error, "could not enqueue the baseline pass");
                false
            }
        }
    }

    /// The `kernel.proc` workflow: runs the pass for the run's day, then publishes and
    /// audits each observation and files it as a question for its partition's audience.
    pub(crate) async fn execute(&self, input: Value) -> std::result::Result<Value, String> {
        let as_of: NaiveDate = input
            .get("asOf")
            .cloned()
            .and_then(|day| serde_json::from_value(day).ok())
            .ok_or_else(|| {
                format!(
                    "a `{PROC_TARGET}` run's input names its day as {{\"asOf\": \"YYYY-MM-DD\"}}"
                )
            })?;
        let outcome = self.pass.run(as_of).await.map_err(|error| error.to_string())?;
        for observation in &outcome.observations {
            self.bus.publish(KernelEvent::Observation {
                id: observation.id.clone(),
                partition: observation.partition.clone(),
            });
            if let Some(trail) = &self.audit {
                let dimensions: Vec<&str> =
                    observation.dimensions.iter().map(|d| d.dimension.as_str()).collect();
                let mut actor = AuditActor::subject(SubjectRef::service(KERNEL_AGENT));
                actor.instance = Some(self.kernel_id.clone());
                let draft = AuditRecordDraft::new(
                    AuditRecordKind::Other(crate::audit::KERNEL_AUDIT_KIND.to_string()),
                    crate::audit::OBSERVATION,
                )
                .actor(actor)
                .ext("id", json!(observation.id))
                .ext("partition", json!(observation.partition))
                .ext("dimensions", json!(dimensions))
                .ext("windows", json!(observation.windows));
                if let Err(error) = trail.record(draft).await {
                    tracing::warn!(error = %error, "could not write a kernel.observation record");
                }
            }
            if let Some(agenda) = self.agenda.get()
                && let Err(error) = agenda.offer(observation).await
            {
                tracing::warn!(observation.id = %observation.id, error = %error, "could not put an observation to its partition's audience");
            }
        }
        Ok(json!({
            "asOf": as_of,
            "partitions": outcome.partitions.len(),
            "days": outcome.days,
            "observations": outcome.observations.iter().map(|o| o.id.clone()).collect::<Vec<_>>(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use chrono::{FixedOffset, TimeZone as _};

    use super::*;
    use crate::proc::{Activity, InMemoryLedgerStore, LedgerKey, LedgerOffset, LedgerSink};

    fn offset() -> FixedOffset {
        FixedOffset::east_opt(3 * 3600).unwrap()
    }

    fn ledger() -> Ledger {
        Ledger::new(Arc::new(InMemoryLedgerStore::new()), LedgerKey::from_bytes([5; 32]))
            .with_offset(LedgerOffset::Fixed(offset()))
    }

    /// Monday 2026-08-31.
    fn start() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 8, 31).unwrap()
    }

    fn day(offset: u64) -> NaiveDate {
        start() + Days::new(offset)
    }

    fn local(day: NaiveDate, minute_of_day: u32) -> DateTime<Utc> {
        let time = day.and_hms_opt(minute_of_day / 60, minute_of_day % 60, 0).unwrap();
        offset().from_local_datetime(&time).unwrap().to_utc()
    }

    /// A day starting at 09:00 with a request, `calls` tool calls spread to the last
    /// activity at `last` (minutes after midnight, local), and a closing request.
    async fn workday(ledger: &Ledger, day: NaiveDate, calls: u32, last: u32) {
        let first = 9 * 60;
        ledger
            .record(Activity::new(LedgerKind::Intent, "harness").at(local(day, first)))
            .await
            .unwrap();
        for n in 1..=calls {
            let at = first + (last - first) * n / (calls + 1);
            ledger
                .record(Activity::new(LedgerKind::ToolCall, "harness").at(local(day, at)))
                .await
                .unwrap();
        }
        ledger
            .record(Activity::new(LedgerKind::Intent, "harness").at(local(day, last)))
            .await
            .unwrap();
    }

    fn weekend(day: NaiveDate) -> bool {
        matches!(chrono::Datelike::weekday(&day), chrono::Weekday::Sat | chrono::Weekday::Sun)
    }

    /// `days` days: four weeks ending at 17:30 with 12 tool calls, then weekdays ending at
    /// 20:30 with `late_calls`; weekends short and unchanged.
    async fn routine(ledger: &Ledger, days: u64, late_calls: u32) {
        for offset in 0..days {
            let day = day(offset);
            if weekend(day) {
                workday(ledger, day, 2, 11 * 60 + 30).await;
            } else if offset < 28 {
                workday(ledger, day, 12, 17 * 60 + 30).await;
            } else {
                workday(ledger, day, late_calls, 20 * 60 + 30).await;
            }
        }
    }

    /// The days the pass observed, one pass per day as the kernel runs it.
    async fn observed_days(pass: &BaselinePass, days: u64) -> Vec<u64> {
        let mut observed = Vec::new();
        for offset in 0..days {
            let outcome = pass.run(day(offset)).await.unwrap();
            for observation in outcome.observations {
                assert!(is_neutral_question(&observation.question), "{}", observation.question);
                observed.push((observation.as_of - start()).num_days() as u64);
            }
        }
        observed
    }

    #[tokio::test]
    async fn one_late_dimension_is_observed_on_its_tenth_day_and_then_cools_down() {
        let ledger = ledger();
        routine(&ledger, 49, 12).await;
        let pass = BaselinePass::new(ledger, BaselinePolicy::default());
        // Both windows deviate from Friday day 32; ten days later the drift is meaningful;
        // seven days after that the cooldown ends.
        assert_eq!(observed_days(&pass, 49).await, [41, 48]);
        let observation =
            &pass.ledger().store().observations(&Default::default()).await.unwrap()[1];
        assert_eq!(observation.dimensions.len(), 1);
        assert_eq!(observation.dimensions[0].dimension, "activity.last_minute");
        assert_eq!(observation.dimensions[0].direction, Direction::Higher);
        assert_eq!(observation.dimensions[0].since, day(32));
        assert_eq!(observation.dimensions[0].days, 10);
        assert_eq!(observation.phrasing, Phrasing::Template);
        assert_eq!(
            observation.question,
            "Your routine has changed over the last two weeks: the day's last activity has been \
             later than usual. Would you like to review what changed?"
        );
    }

    #[tokio::test]
    async fn two_dimensions_are_observed_at_once_and_repeat_after_the_cooldown() {
        let ledger = ledger();
        routine(&ledger, 42, 24).await;
        let pass = BaselinePass::new(ledger, BaselinePolicy::default());
        assert_eq!(observed_days(&pass, 42).await, [32, 39]);
    }

    #[tokio::test]
    async fn one_catch_up_pass_finds_what_daily_passes_find() {
        let ledger = ledger();
        routine(&ledger, 42, 24).await;
        let pass = BaselinePass::new(ledger, BaselinePolicy::default());
        let outcome = pass.run(day(41)).await.unwrap();
        assert_eq!(outcome.days, 42);
        let days: Vec<NaiveDate> = outcome.observations.iter().map(|o| o.as_of).collect();
        assert_eq!(days, [day(32), day(39)]);
        let again = pass.run(day(41)).await.unwrap();
        assert!(again.observations.is_empty() && again.days == 0, "a day is tested once");
    }

    #[tokio::test]
    async fn a_steady_routine_and_the_kernels_own_rows_are_never_observed() {
        let ledger = ledger();
        for offset in 0..42u64 {
            let day = day(offset);
            workday(&ledger, day, 12, 17 * 60 + 30).await;
            if offset >= 28 {
                // The kernel's own work late at night moves nothing.
                for minute in [23 * 60 + 50, 23 * 60 + 55] {
                    ledger
                        .record(
                            Activity::new(LedgerKind::Process, KERNEL_AGENT).at(local(day, minute)),
                        )
                        .await
                        .unwrap();
                }
            }
        }
        let pass = BaselinePass::new(ledger, BaselinePolicy::default());
        assert!(observed_days(&pass, 42).await.is_empty());
    }

    /// The export `GET /kernel/proc/ledger?format=jsonl` writes is the input
    /// `cargo adk eval --ledger` reads, and its gate finds the same drift.
    #[tokio::test]
    async fn the_export_is_what_cargo_adk_eval_reads() {
        let ledger = ledger();
        routine(&ledger, 42, 24).await;
        let rows = ledger.store().list(&LedgerQuery::default()).await.unwrap();
        let export: String =
            rows.iter().map(|row| format!("{}\n", row.to_jsonl().unwrap())).collect();
        let parsed = adk_eval::ledger::parse_jsonl(&export).unwrap();
        assert_eq!(parsed.len(), rows.len());
        assert_eq!(parsed[0], row(&rows[0]), "every column reads back");
        let assessment = adk_eval::ledger::assess(
            &adk_eval::ledger::daily_series(&parsed),
            &DeviationConfig::default(),
        )
        .unwrap();
        assert!(assessment.meaningful, "the gate fails on the late finishes");
        assert!(assessment.report.deviating().contains(&"activity.last_minute"));
    }

    struct Scripted {
        reply: &'static str,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl ObservationPhraser for Scripted {
        fn name(&self) -> &str {
            "scripted"
        }

        async fn phrase(&self, detection: &Detection, budget: &RunBudget) -> Result<String> {
            assert!(!detection.prompt().contains("hmac"), "the model sees no row");
            assert_eq!(budget.max_model_calls, Some(1));
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.reply.to_string())
        }
    }

    #[tokio::test]
    async fn the_model_words_within_the_budget_and_a_diagnosis_falls_back_to_the_template() {
        for (reply, expected) in [
            (
                "Your evenings have been running later these past two weeks. Would you like to \
                 review what changed?",
                Phrasing::Model { model: "scripted".into() },
            ),
            ("You are burning out from working late.", Phrasing::Template),
        ] {
            let ledger = ledger();
            routine(&ledger, 42, 24).await;
            let phraser = Arc::new(Scripted { reply, calls: AtomicUsize::new(0) });
            let pass = BaselinePass::new(
                ledger,
                BaselinePolicy::default().with_daily_budget(RunBudget::new().max_model_calls(1)),
            )
            .with_phraser(phraser.clone());
            // One catch-up pass finds two observations; the budget words one of them.
            let outcome = pass.run(day(41)).await.unwrap();
            assert_eq!(outcome.observations.len(), 2);
            assert_eq!(outcome.observations[0].phrasing, expected);
            assert_eq!(outcome.observations[1].phrasing, Phrasing::Template);
            assert!(outcome.observations.iter().all(|o| is_neutral_question(&o.question)));
            assert_eq!(phraser.calls.load(Ordering::SeqCst), 1);
        }
    }
}
