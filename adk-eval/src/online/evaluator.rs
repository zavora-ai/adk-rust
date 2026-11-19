//! The online evaluator: sampling hooks on the run path, scoring on a background worker.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use adk_core::{
    BudgetTracker, Content, Event, GovernanceControl, InvocationContext, Llm, ModelCallScope, Part,
    RunBudget, RunConfig, SpendLedger,
};
use adk_plugin::{Plugin, PluginConfig};
use serde_json::Value;
use tokio::sync::{Notify, mpsc};
use tracing::Instrument;

use super::sampling::{RunOutcome, RunTrace, SamplingPolicy};
use super::store::{InMemoryOnlineEvalStore, OnlineEvalRecord, OnlineEvalStore};
use crate::conversation_scorer::{ConversationScorer, ConversationScorerConfig};
use crate::criteria::EvaluationCriteria;
use crate::error::{EvalError, Result};
use crate::evaluator::{EvaluationConfig, Evaluator};
use crate::report::{Failure, TurnResult};
use crate::schema::{ContentData, Turn};
use crate::structured_judge::StructuredJudge;
use crate::trace_analyzer::TraceAnalyzer;

/// The agent online evaluation's judge calls are attributed to in the spend ledger.
pub const ONLINE_EVAL_AGENT_NAME: &str = "online-eval";

/// The judge model calls an [`OnlineEvalConfig`] allows unless it sets another budget.
pub const DEFAULT_ONLINE_EVAL_MAX_MODEL_CALLS: u64 = 1_000;

/// What online evaluation samples, how it scores a run, and what it may spend.
///
/// # Example
///
/// ```
/// use adk_core::RunBudget;
/// use adk_eval::online::{OnlineEvalConfig, SamplingPolicy};
/// use adk_eval::{EvaluationCriteria, Rubric};
///
/// let criteria = EvaluationCriteria::default()
///     .with_rubrics(0.7, vec![Rubric::new("Grounded", "Answers only from tool results")]);
/// let config = OnlineEvalConfig::new(SamplingPolicy::rate(0.05)?)
///     .with_criteria(criteria)
///     .with_budget(RunBudget::new().max_model_calls(500).max_cost_usd(5.0));
/// assert_eq!(config.queue_capacity, 64);
/// # Ok::<(), adk_eval::EvalError>(())
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct OnlineEvalConfig {
    /// Which runs are sampled.
    pub sampling: SamplingPolicy,
    /// Reference-free criteria: `rubric_quality_score` with `rubric_config`, `safety_score`
    /// and `hallucination_score`.
    pub criteria: EvaluationCriteria,
    /// Lowest acceptable [`TraceAnalyzer`] efficiency score of a run's tool calls.
    pub min_trajectory_efficiency: Option<f64>,
    /// Thread-level criteria over the whole session, with their thresholds.
    pub thread: Option<ConversationScorerConfig>,
    /// The judge's budget for the evaluator's lifetime. Defaults to
    /// [`DEFAULT_ONLINE_EVAL_MAX_MODEL_CALLS`] model calls.
    pub budget: RunBudget,
    /// Finished runs that may wait for the worker; a run that finds the queue full is
    /// dropped, never waited for. Defaults to 64.
    pub queue_capacity: usize,
    /// Events kept per sampled run; later events are not buffered. Defaults to 1,024.
    pub max_events_per_run: usize,
}

impl OnlineEvalConfig {
    /// A configuration that samples with `sampling` and scores nothing yet.
    pub fn new(sampling: SamplingPolicy) -> Self {
        Self {
            sampling,
            criteria: EvaluationCriteria::default(),
            min_trajectory_efficiency: None,
            thread: None,
            budget: RunBudget::new().max_model_calls(DEFAULT_ONLINE_EVAL_MAX_MODEL_CALLS),
            queue_capacity: 64,
            max_events_per_run: 1_024,
        }
    }

    /// Scores runs with the reference-free LLM-judged `criteria`.
    pub fn with_criteria(mut self, criteria: EvaluationCriteria) -> Self {
        self.criteria = criteria;
        self
    }

    /// Fails a run whose tool-call efficiency is below `min`, from 0.0 to 1.0.
    pub fn with_min_trajectory_efficiency(mut self, min: f64) -> Self {
        self.min_trajectory_efficiency = Some(min);
        self
    }

    /// Scores the whole session after each sampled run, failing it below `thresholds`.
    pub fn with_thread(mut self, thresholds: ConversationScorerConfig) -> Self {
        self.thread = Some(thresholds);
        self
    }

    /// Sets the judge's budget for the evaluator's lifetime.
    pub fn with_budget(mut self, budget: RunBudget) -> Self {
        self.budget = budget;
        self
    }

    /// Sets how many finished runs may wait for the worker.
    pub fn with_queue_capacity(mut self, capacity: usize) -> Self {
        self.queue_capacity = capacity;
        self
    }

    /// Sets how many events are kept per sampled run.
    pub fn with_max_events_per_run(mut self, max: usize) -> Self {
        self.max_events_per_run = max;
        self
    }

    fn judged_per_run(&self) -> bool {
        self.criteria.rubric_quality_score.is_some()
            || self.criteria.safety_score.is_some()
            || self.criteria.hallucination_score.is_some()
    }

    fn needs_judge(&self) -> bool {
        self.judged_per_run() || self.thread.is_some()
    }

    fn validate(&self, has_judge: bool) -> Result<()> {
        let refuse = |message: String| Err(EvalError::ConfigError(message));
        let referenced: Vec<&str> = [
            ("tool_trajectory_score", self.criteria.tool_trajectory_score),
            ("response_similarity", self.criteria.response_similarity),
            ("semantic_match_score", self.criteria.semantic_match_score),
        ]
        .into_iter()
        .filter_map(|(name, threshold)| threshold.map(|_| name))
        .collect();
        if !referenced.is_empty() {
            return refuse(format!(
                "{} compare a run with an expected answer, which production runs do not have; \
                 use rubric_quality_score, safety_score, hallucination_score, \
                 min_trajectory_efficiency or thread-level criteria",
                referenced.join(", ")
            ));
        }
        if !self.criteria.custom.is_empty() {
            return refuse("custom criteria are not scored; remove them".to_string());
        }
        if self.criteria.rubric_quality_score.is_some()
            && self.criteria.rubric_config.as_ref().is_none_or(|config| config.rubrics.is_empty())
        {
            return refuse(
                "rubric_quality_score needs rubrics; add them with EvaluationCriteria::with_rubrics"
                    .to_string(),
            );
        }
        if !self.needs_judge() && self.min_trajectory_efficiency.is_none() {
            return refuse(
                "no online criterion is configured, so no run could fail; set a judged \
                 criterion, min_trajectory_efficiency or thread-level criteria"
                    .to_string(),
            );
        }
        if let Some(min) = self.min_trajectory_efficiency
            && !(0.0..=1.0).contains(&min)
        {
            return refuse(format!(
                "min_trajectory_efficiency must be between 0.0 and 1.0, got {min}"
            ));
        }
        if self.needs_judge() && !has_judge {
            return refuse(
                "the configured criteria need an LLM judge; call OnlineEvaluatorBuilder::judge"
                    .to_string(),
            );
        }
        if self.needs_judge() && self.budget.is_unlimited() {
            return refuse(
                "online evaluation needs a judge budget; set max_model_calls or max_cost_usd with \
                 OnlineEvalConfig::with_budget"
                    .to_string(),
            );
        }
        if self.queue_capacity == 0 || self.max_events_per_run == 0 {
            return refuse("queue_capacity and max_events_per_run must be at least 1".to_string());
        }
        Ok(())
    }
}

/// What happened to a run handed to the evaluator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Submission {
    /// The run waits for the worker.
    Queued,
    /// The sampling policy did not select the run.
    NotSampled,
    /// The run was selected but will not be evaluated.
    Dropped(DropReason),
}

/// Why a selected run was not queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DropReason {
    /// The queue was full; the run is not waited for.
    QueueFull,
    /// A kill switch of the evaluator or of the run is frozen.
    Frozen,
    /// The judge budget is spent.
    BudgetSpent,
    /// Too many sampled runs were still in flight to buffer another.
    TooManyInFlight,
    /// The evaluator was shut down.
    Stopped,
}

/// Counters of an online evaluator since it started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct OnlineEvalStats {
    /// Runs the evaluator saw.
    pub observed: u64,
    /// Runs the sampling policy selected.
    pub sampled: u64,
    /// Selected runs queued for the worker.
    pub queued: u64,
    /// Selected runs dropped before the worker ([`DropReason`]).
    pub dropped: u64,
    /// Runs evaluated and recorded.
    pub evaluated: u64,
    /// Evaluated runs that passed.
    pub passed: u64,
    /// Evaluated runs that failed a criterion.
    pub failed: u64,
    /// Queued runs not evaluated because a kill switch or the budget refused.
    pub refused: u64,
    /// Queued runs whose evaluation or record failed for another reason.
    pub errors: u64,
}

#[derive(Default)]
struct Counters {
    observed: AtomicU64,
    sampled: AtomicU64,
    queued: AtomicU64,
    dropped: AtomicU64,
    evaluated: AtomicU64,
    passed: AtomicU64,
    failed: AtomicU64,
    refused: AtomicU64,
    errors: AtomicU64,
}

fn bump(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::Relaxed);
}

/// Builds and starts an [`OnlineEvaluator`].
pub struct OnlineEvaluatorBuilder {
    config: OnlineEvalConfig,
    judge: Option<Arc<dyn Llm>>,
    store: Option<Arc<dyn OnlineEvalStore>>,
    spend: Option<(Arc<dyn SpendLedger>, String)>,
    governance: Vec<GovernanceControl>,
}

impl OnlineEvaluatorBuilder {
    /// The model that judges LLM-judged and thread-level criteria.
    pub fn judge(mut self, model: Arc<dyn Llm>) -> Self {
        self.judge = Some(model);
        self
    }

    /// Where records are kept. Defaults to an [`InMemoryOnlineEvalStore`] of 1,000 records.
    pub fn store(mut self, store: Arc<dyn OnlineEvalStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// Reserves and commits judge spend in `ledger`, attributed to `org` and the agent
    /// [`ONLINE_EVAL_AGENT_NAME`].
    pub fn spend_ledger(mut self, ledger: Arc<dyn SpendLedger>, org: impl Into<String>) -> Self {
        self.spend = Some((ledger, org.into()));
        self
    }

    /// Stops sampling and judging while `control` is frozen (repeatable). Pass the runner's
    /// [`Runner::governance`](https://docs.rs/adk-runner) to share its kill switch; the
    /// kill switches of each run are honoured as well.
    pub fn governance(mut self, control: GovernanceControl) -> Self {
        self.governance.push(control);
        self
    }

    /// Starts the background worker on the current Tokio runtime.
    ///
    /// # Errors
    ///
    /// Returns [`EvalError::ConfigError`] when the configuration cannot score a run (a
    /// criterion that needs an expected answer, nothing to score, a judged criterion without
    /// a judge or a budget) or when no Tokio runtime is running.
    pub fn start(self) -> Result<OnlineEvaluator> {
        let Self { config, judge, store, spend, governance } = self;
        config.validate(judge.is_some())?;
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| {
            EvalError::ConfigError(
                "start the online evaluator inside a Tokio runtime; its worker runs there"
                    .to_string(),
            )
        })?;

        let tracker = Arc::new(BudgetTracker::new(config.budget.clone()));
        let mut scope = match &spend {
            Some((ledger, org)) => ModelCallScope::for_run(
                &RunConfig::builder().spend_ledger(ledger.clone()).build(),
                org,
                ONLINE_EVAL_AGENT_NAME,
            ),
            None => ModelCallScope::new(),
        }
        .with_budget(tracker.clone());
        for control in &governance {
            scope = scope.with_governance(control.clone());
        }

        let evaluation = EvaluationConfig::with_criteria(config.criteria.clone());
        let scorer = match &judge {
            Some(judge) => Evaluator::with_llm_judge(evaluation, judge.clone()),
            None => Evaluator::new(evaluation),
        };
        let thread_scorer = match (&config.thread, &judge) {
            (Some(thresholds), Some(judge)) => {
                #[cfg(feature = "embedding")]
                let scorer = ConversationScorer::with_config(
                    StructuredJudge::new(judge.clone()),
                    None,
                    thresholds.clone(),
                );
                #[cfg(not(feature = "embedding"))]
                let scorer = ConversationScorer::with_config(
                    StructuredJudge::new(judge.clone()),
                    thresholds.clone(),
                );
                Some(scorer)
            }
            _ => None,
        };

        let shared = Arc::new(Shared {
            counters: Counters::default(),
            outstanding: AtomicUsize::new(0),
            idle: Notify::new(),
            tracker,
            judged: config.needs_judge(),
            governance,
            store: store.unwrap_or_else(|| Arc::new(InMemoryOnlineEvalStore::new(1_000))),
        });
        let (sender, receiver) = mpsc::channel(config.queue_capacity);
        let worker = Worker {
            shared: shared.clone(),
            scorer,
            judged_per_run: config.judged_per_run(),
            min_trajectory_efficiency: config.min_trajectory_efficiency,
            thread_scorer,
            scope,
        };
        let task = runtime.spawn(worker.run(receiver));
        tracing::info!(
            eval.online.rate = config.sampling.rate,
            eval.online.queue = config.queue_capacity,
            "online evaluation started"
        );
        Ok(OnlineEvaluator {
            handle: Arc::new(Handle {
                max_in_flight: config.queue_capacity.saturating_mul(4).max(16),
                sampling: config.sampling,
                thread_level: config.thread.is_some(),
                max_events_per_run: config.max_events_per_run,
                sender: Mutex::new(Some(sender)),
                in_flight: Mutex::new(HashMap::new()),
                in_flight_count: AtomicUsize::new(0),
                worker: Mutex::new(Some(task)),
                shared,
            }),
        })
    }
}

/// What the worker and the handles share.
struct Shared {
    counters: Counters,
    /// Runs queued and not yet finished by the worker.
    outstanding: AtomicUsize,
    idle: Notify,
    tracker: Arc<BudgetTracker>,
    judged: bool,
    governance: Vec<GovernanceControl>,
    store: Arc<dyn OnlineEvalStore>,
}

impl Shared {
    fn frozen(&self, run_controls: &[GovernanceControl]) -> bool {
        self.governance.iter().chain(run_controls).any(|control| control.check().is_err())
    }

    fn budget_spent(&self) -> bool {
        self.judged
            && (self.tracker.check().is_err()
                || self.tracker.remaining().max_model_calls == Some(0))
    }

    fn finished_one(&self) {
        self.outstanding.fetch_sub(1, Ordering::SeqCst);
        self.idle.notify_waiters();
    }
}

/// A sampled run whose events are being buffered.
struct InFlightRun {
    metadata: HashMap<String, String>,
    events: Vec<Event>,
}

struct Handle {
    sampling: SamplingPolicy,
    thread_level: bool,
    max_events_per_run: usize,
    max_in_flight: usize,
    sender: Mutex<Option<mpsc::Sender<RunTrace>>>,
    in_flight: Mutex<HashMap<String, InFlightRun>>,
    /// Lets runs that are not sampled skip the in-flight lock on every event.
    in_flight_count: AtomicUsize,
    worker: Mutex<Option<tokio::task::JoinHandle<()>>>,
    shared: Arc<Shared>,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Judges a sampled fraction of finished runs on a background worker.
///
/// Install [`plugin`](Self::plugin) on a runner's plugin manager, or hand finished runs to
/// [`submit`](Self::submit). See the [module documentation](crate::online).
#[derive(Clone)]
pub struct OnlineEvaluator {
    handle: Arc<Handle>,
}

impl std::fmt::Debug for OnlineEvaluator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnlineEvaluator")
            .field("sampling", &self.handle.sampling)
            .field("stats", &self.stats())
            .finish()
    }
}

impl OnlineEvaluator {
    /// Starts building an evaluator with `config`.
    pub fn builder(config: OnlineEvalConfig) -> OnlineEvaluatorBuilder {
        OnlineEvaluatorBuilder {
            config,
            judge: None,
            store: None,
            spend: None,
            governance: Vec::new(),
        }
    }

    /// A plugin that samples the runs of the runner it is installed on.
    ///
    /// `before_run` decides whether the run is sampled, `on_event` buffers a sampled run's
    /// complete events, and `after_run` queues the finished run without waiting for the
    /// worker. A run that is not sampled costs one atomic load per event.
    pub fn plugin(&self) -> Plugin {
        let before = self.clone();
        let during = self.clone();
        let after = self.clone();
        Plugin::new(PluginConfig {
            name: "online-eval".to_string(),
            before_run: Some(Box::new(move |ctx| {
                let this = before.clone();
                Box::pin(async move {
                    this.begin_run(ctx.as_ref());
                    Ok(None)
                })
            })),
            on_event: Some(Box::new(move |ctx, event| {
                let this = during.clone();
                Box::pin(async move {
                    this.record_event(ctx.invocation_id(), event);
                    Ok(None)
                })
            })),
            after_run: Some(Box::new(move |ctx| {
                let this = after.clone();
                Box::pin(async move {
                    this.finish_run(ctx.as_ref());
                })
            })),
            ..Default::default()
        })
    }

    /// Hands a finished run to the evaluator, which evaluates it when the sampling policy
    /// selects it. Never waits: a full queue drops the run.
    pub fn submit(&self, trace: RunTrace) -> Submission {
        bump(&self.handle.shared.counters.observed);
        if !self.handle.sampling.admits(&trace) {
            return Submission::NotSampled;
        }
        self.enqueue(trace)
    }

    /// Counters since the evaluator started.
    pub fn stats(&self) -> OnlineEvalStats {
        let c = &self.handle.shared.counters;
        let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        OnlineEvalStats {
            observed: load(&c.observed),
            sampled: load(&c.sampled),
            queued: load(&c.queued),
            dropped: load(&c.dropped),
            evaluated: load(&c.evaluated),
            passed: load(&c.passed),
            failed: load(&c.failed),
            refused: load(&c.refused),
            errors: load(&c.errors),
        }
    }

    /// The judge's budget tracker, for its usage.
    pub fn budget(&self) -> &Arc<BudgetTracker> {
        &self.handle.shared.tracker
    }

    /// Where records are kept.
    pub fn store(&self) -> &Arc<dyn OnlineEvalStore> {
        &self.handle.shared.store
    }

    /// Waits until every queued run has been evaluated or refused.
    pub async fn flush(&self) {
        loop {
            // Created before the check, so a notification between the two is not lost.
            let notified = self.handle.shared.idle.notified();
            if self.handle.shared.outstanding.load(Ordering::SeqCst) == 0 {
                return;
            }
            notified.await;
        }
    }

    /// Stops accepting runs, lets the worker finish the queue, and waits for it.
    pub async fn shutdown(&self) {
        drop(locked(&self.handle.sender).take());
        let worker = locked(&self.handle.worker).take();
        if let Some(worker) = worker {
            let _ = worker.await;
        }
    }

    fn enqueue(&self, trace: RunTrace) -> Submission {
        let shared = &self.handle.shared;
        bump(&shared.counters.sampled);
        let refused = if shared.frozen(&trace.governance) {
            Some(DropReason::Frozen)
        } else if shared.budget_spent() {
            Some(DropReason::BudgetSpent)
        } else {
            None
        };
        if let Some(reason) = refused {
            return self.drop_run(&trace, reason);
        }
        let sender = locked(&self.handle.sender).clone();
        let Some(sender) = sender else {
            return self.drop_run(&trace, DropReason::Stopped);
        };
        shared.outstanding.fetch_add(1, Ordering::SeqCst);
        match sender.try_send(trace) {
            Ok(()) => {
                bump(&shared.counters.queued);
                Submission::Queued
            }
            Err(error) => {
                shared.finished_one();
                let (trace, reason) = match error {
                    mpsc::error::TrySendError::Full(trace) => (trace, DropReason::QueueFull),
                    mpsc::error::TrySendError::Closed(trace) => (trace, DropReason::Stopped),
                };
                self.drop_run(&trace, reason)
            }
        }
    }

    fn drop_run(&self, trace: &RunTrace, reason: DropReason) -> Submission {
        bump(&self.handle.shared.counters.dropped);
        tracing::debug!(
            invocation.id = %trace.invocation_id,
            reason = ?reason,
            "online evaluation dropped a sampled run"
        );
        Submission::Dropped(reason)
    }

    fn begin_run(&self, ctx: &dyn InvocationContext) {
        bump(&self.handle.shared.counters.observed);
        let metadata: HashMap<String, String> = ctx
            .request_metadata()
            .into_iter()
            .map(|(key, value)| match value {
                Value::String(text) => (key, text),
                other => (key, other.to_string()),
            })
            .collect();
        if !self.handle.sampling.admits_run(ctx.agent_name(), &metadata, ctx.invocation_id()) {
            return;
        }
        let mut in_flight = locked(&self.handle.in_flight);
        if in_flight.len() >= self.handle.max_in_flight {
            drop(in_flight);
            bump(&self.handle.shared.counters.sampled);
            bump(&self.handle.shared.counters.dropped);
            tracing::debug!(
                invocation.id = %ctx.invocation_id(),
                "online evaluation dropped a sampled run: too many runs in flight"
            );
            return;
        }
        in_flight
            .insert(ctx.invocation_id().to_string(), InFlightRun { metadata, events: Vec::new() });
        self.handle.in_flight_count.store(in_flight.len(), Ordering::SeqCst);
    }

    fn record_event(&self, invocation_id: &str, event: Event) {
        if self.handle.in_flight_count.load(Ordering::SeqCst) == 0 || event.llm_response.partial {
            return;
        }
        if let Some(run) = locked(&self.handle.in_flight).get_mut(invocation_id)
            && run.events.len() < self.handle.max_events_per_run
        {
            run.events.push(event);
        }
    }

    fn finish_run(&self, ctx: &dyn InvocationContext) {
        let run = {
            let mut in_flight = locked(&self.handle.in_flight);
            let run = in_flight.remove(ctx.invocation_id());
            self.handle.in_flight_count.store(in_flight.len(), Ordering::SeqCst);
            run
        };
        let Some(run) = run else { return };
        let outcome = RunOutcome::of(&run.events, ctx.is_cancelled());
        if !self.handle.sampling.admits_outcome(outcome) {
            return;
        }
        let trace = RunTrace {
            app_name: ctx.app_name().to_string(),
            session_id: ctx.session_id().to_string(),
            invocation_id: ctx.invocation_id().to_string(),
            agent_name: ctx.agent_name().to_string(),
            metadata: run.metadata,
            user_content: ctx.user_content().clone(),
            events: run.events,
            outcome,
            thread: if self.handle.thread_level {
                ctx.session().conversation_history()
            } else {
                Vec::new()
            },
            governance: ctx.run_config().governance.clone(),
            finished_at: chrono::Utc::now(),
        };
        self.enqueue(trace);
    }
}

/// Scores queued runs one at a time.
struct Worker {
    shared: Arc<Shared>,
    scorer: Evaluator,
    judged_per_run: bool,
    min_trajectory_efficiency: Option<f64>,
    thread_scorer: Option<ConversationScorer>,
    scope: ModelCallScope,
}

impl Worker {
    async fn run(self, mut receiver: mpsc::Receiver<RunTrace>) {
        while let Some(trace) = receiver.recv().await {
            self.evaluate_and_record(trace).await;
            self.shared.finished_one();
        }
        tracing::debug!("online evaluation worker stopped");
    }

    async fn evaluate_and_record(&self, trace: RunTrace) {
        let counters = &self.shared.counters;
        let mut scope = self.scope.clone();
        for control in &trace.governance {
            scope = scope.with_governance(control.clone());
        }
        if let Err(error) = scope.check_governance() {
            bump(&counters.refused);
            tracing::warn!(invocation.id = %trace.invocation_id, error = %error, "online evaluation refused by a kill switch");
            return;
        }
        if self.shared.budget_spent() {
            bump(&counters.refused);
            tracing::warn!(invocation.id = %trace.invocation_id, "online evaluation refused: the judge budget is spent");
            return;
        }

        let span = tracing::info_span!(
            "adk.eval.online",
            "agent.name" = %trace.agent_name,
            "invocation.id" = %trace.invocation_id,
            "gen_ai.conversation.id" = %trace.session_id,
            "adk.run.outcome" = ?trace.outcome,
            "adk.eval.passed" = tracing::field::Empty,
            "otel.kind" = "internal",
        );
        let calls_before = self.shared.tracker.usage().model_calls;
        let result = scope.within(self.evaluate(&trace)).instrument(span.clone()).await;
        match result {
            Ok(mut record) => {
                record.judge_model_calls =
                    self.shared.tracker.usage().model_calls.saturating_sub(calls_before);
                emit(&span, &record);
                bump(&counters.evaluated);
                bump(if record.passed { &counters.passed } else { &counters.failed });
                if let Err(error) = self.shared.store.append(&record).await {
                    bump(&counters.errors);
                    tracing::warn!(error = %error, "online evaluation record was not stored");
                }
            }
            Err(error) if error.is_refusal() => {
                bump(&counters.refused);
                tracing::warn!(invocation.id = %trace.invocation_id, error = %error, "online evaluation stopped by a refusal");
            }
            Err(error) => {
                bump(&counters.errors);
                tracing::warn!(invocation.id = %trace.invocation_id, error = %error, "online evaluation failed");
            }
        }
    }

    async fn evaluate(&self, trace: &RunTrace) -> Result<OnlineEvalRecord> {
        let (actual_response, actual_tool_calls) = self.scorer.extract_from_events(&trace.events);
        let user_text: String = trace.user_content.parts.iter().filter_map(Part::text).collect();
        let mut scores: BTreeMap<String, f64> = BTreeMap::new();
        let mut failures: Vec<Failure> = Vec::new();

        if self.judged_per_run {
            // The judges take the user message as context; production runs answer from tool
            // results, so those are part of what the answer is judged against.
            let turn = Turn {
                invocation_id: trace.invocation_id.clone(),
                user_content: ContentData::text(&judge_context(&user_text, &trace.events)),
                final_response: None,
                intermediate_data: None,
                expected_vision: None,
            };
            let result = TurnResult {
                invocation_id: trace.invocation_id.clone(),
                actual_response: actual_response.clone(),
                expected_response: None,
                actual_tool_calls,
                expected_tool_calls: Vec::new(),
                actual_vision: None,
                scores: HashMap::new(),
            };
            let (turn_scores, turn_failures) = self.scorer.score_turn(&turn, &result).await?;
            scores.extend(turn_scores);
            failures.extend(turn_failures);
        }

        let trace_analysis = self.min_trajectory_efficiency.map(|min| {
            let analysis = TraceAnalyzer::new().analyze(&trace.events);
            scores.insert("trajectory_efficiency".to_string(), analysis.efficiency_score);
            if analysis.efficiency_score < min {
                let found = analysis
                    .diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic.description.as_str())
                    .collect::<Vec<_>>()
                    .join("; ");
                failures.push(
                    Failure::new(
                        "trajectory_efficiency",
                        number(min),
                        number(analysis.efficiency_score),
                        analysis.efficiency_score,
                        min,
                    )
                    .with_details(&format!(
                        "{} of {} tool calls were useful: {found}",
                        analysis.useful_tool_calls, analysis.total_tool_calls
                    )),
                );
            }
            analysis
        });

        let thread = match &self.thread_scorer {
            Some(scorer) => {
                let conversation = if trace.thread.is_empty() {
                    let mut turns = vec![trace.user_content.clone()];
                    if let Some(answer) = &actual_response {
                        turns.push(Content::new("model").with_text(answer));
                    }
                    turns
                } else {
                    trace.thread.clone()
                };
                let goal = conversation
                    .iter()
                    .find(|content| content.role == "user")
                    .map(|content| content.parts.iter().filter_map(Part::text).collect::<String>())
                    .unwrap_or_else(|| user_text.clone());
                let metrics = scorer.score(&conversation, &goal).await?;
                let config = scorer.config();
                for (name, score, threshold) in [
                    (
                        "thread_context_retention",
                        metrics.context_retention,
                        config.context_retention_threshold,
                    ),
                    (
                        "thread_goal_completion",
                        metrics.goal_completion,
                        config.goal_completion_threshold,
                    ),
                    ("thread_coherence", metrics.coherence, config.coherence_threshold),
                    ("thread_topic_drift", metrics.topic_drift, config.topic_drift_threshold),
                ] {
                    scores.insert(name.to_string(), score);
                    if score < threshold {
                        failures.push(
                            Failure::new(name, number(threshold), number(score), score, threshold)
                                .with_details(&format!(
                                    "the session scored {score:.2} against a threshold of {threshold:.2}"
                                )),
                        );
                    }
                }
                Some(metrics)
            }
            None => None,
        };

        Ok(OnlineEvalRecord {
            id: uuid::Uuid::new_v4().to_string(),
            evaluated_at: chrono::Utc::now(),
            finished_at: trace.finished_at,
            app_name: trace.app_name.clone(),
            session_id: trace.session_id.clone(),
            invocation_id: trace.invocation_id.clone(),
            agent_name: trace.agent_name.clone(),
            outcome: trace.outcome,
            passed: failures.is_empty(),
            scores,
            failures,
            trace_analysis,
            thread,
            judge_model_calls: 0,
        })
    }
}

/// Longest tool result, in characters, the judges are shown.
const MAX_TOOL_RESULT_CHARS: usize = 2_000;

/// The user message followed by the tool results the run received, each cut to
/// [`MAX_TOOL_RESULT_CHARS`].
fn judge_context(user_text: &str, events: &[Event]) -> String {
    let results: Vec<String> = events
        .iter()
        .filter_map(Event::content)
        .flat_map(|content| &content.parts)
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, .. } => {
                let response = function_response.response.to_string();
                let shown: String = response.chars().take(MAX_TOOL_RESULT_CHARS).collect();
                Some(format!("- {}: {shown}", function_response.name))
            }
            _ => None,
        })
        .collect();
    if results.is_empty() {
        return user_text.to_string();
    }
    format!("{user_text}\n\nTool results the agent received:\n{}", results.join("\n"))
}

/// Emits one `gen_ai.evaluation.result` event per scored criterion in `span`.
fn emit(span: &tracing::Span, record: &OnlineEvalRecord) {
    span.record("adk.eval.passed", record.passed);
    for (name, score) in &record.scores {
        let failure = record.failures.iter().find(|failure| &failure.criterion == name);
        let label = if failure.is_some() { "fail" } else { "pass" };
        let explanation = failure.and_then(|failure| failure.details.as_deref()).unwrap_or("");
        // The field names are spelled out because tracing takes no constants; they are the
        // values of `attributes`.
        tracing::info!(
            parent: span,
            gen_ai.evaluation.name = %name,
            gen_ai.evaluation.score.value = *score,
            gen_ai.evaluation.score.label = label,
            gen_ai.evaluation.explanation = explanation,
            "gen_ai.evaluation.result"
        );
    }
}

fn number(value: f64) -> Value {
    serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number)
}
