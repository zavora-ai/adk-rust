//! Background sub-agents as durable runs: [`DurableSubagents`], the
//! [`SubagentHost`] over any [`DurableRunStore`] and the [`DurableWorker`] that claims its
//! queue.
//!
//! A sub-agent is a run of its target in a session of its own, `subagent-<id>`; every run
//! of it carries the [`SubagentLaunch`] under `subagent`, and the budget slice and the
//! parent's tools under `runLimits`, which the worker applies (see
//! [Run limits](super::worker#run-limits)). Its runs share the concurrency key
//! `subagent:<id>`, so a follow-up runs after the run before it.
//!
//! | Operation | What happens |
//! |---|---|
//! | `start` | Checks depth, running sub-agents and target; carves the budget slice from the parent's run; forks the parent's session when asked; enqueues the first run through the [`SubagentBackend`] |
//! | `send` | Steers the sub-agent's running turn; queues a follow-up run when none runs here, or when the turn ends without taking the message |
//! | `cancel` | Cancels every unfinished run of it; its parent is not told |
//! | A run finishes ([`DurableSubagents::run_finished`]) | The backend reports the status; the parent is told, by steering its running turn or by a new run of its session keyed `subagent:<id>:<run>:<status>`, so it is told once |
//! | A run parks on a held call ([`DurableSubagents::run_parked`]) | The backend reports `waitingForHuman`; a parent turn in flight is told |
//! | Start ([`DurableSubagents::reconcile`]) | Tells every parent about finished runs it was not told about, after a crash |
//!
//! [`SubagentHooks`] wires the last three into a plain [`DurableWorker`]; `adk-kernel` calls
//! them from its own worker hooks.
//!
//! # Example
//!
//! ```rust,no_run
//! use std::sync::Arc;
//! use adk_core::{InMemoryRunStore, RunBudget};
//! use adk_server::background::{
//!     DurableSubagents, DurableWorker, RunStoreBackend, SubagentConfig, SubagentHooks,
//! };
//!
//! # fn example(runner: Arc<adk_runner::Runner>) -> adk_core::Result<()> {
//! let runs = Arc::new(InMemoryRunStore::new());
//! let backend = RunStoreBackend::new(runs.clone()).with_runner(&runner);
//! let subagents = DurableSubagents::new(
//!     runs.clone(),
//!     runner.session_service().clone(),
//!     Arc::new(backend),
//!     SubagentConfig::default().with_default_budget(RunBudget::new().max_model_calls(20)),
//! );
//! let worker = DurableWorker::new(runs)
//!     .with_agent("assistant", runner)
//!     .with_hooks(Arc::new(SubagentHooks::new(subagents.clone())))
//!     .spawn();
//! # let _ = worker;
//! # Ok(())
//! # }
//! ```

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use adk_core::subagent::{
    FORK_INVOCATION_PREFIX, RUN_LIMITS_INPUT_KEY, RunLimits, SUBAGENT_FINISHED_CODE,
    SUBAGENT_INPUT_KEY, notice_header, subagent_not_found, subagent_notice, subagent_refused,
};
use adk_core::{
    AdkError, AdkIdentity, AppName, Content, DurableRunStore, Enqueued, ErrorCategory,
    ErrorComponent, FollowUp, NewRun, OnConflict, Part, Result, RunBudget, RunFilter, RunRecord,
    RunState, STEER_NO_ACTIVE_RUN_CODE, SessionId, SteerOutcome, SteerReceipt, SubagentHost,
    SubagentLaunch, SubagentParent, SubagentRecord, SubagentScope, SubagentStart, SubagentStatus,
    SubagentTarget, UserId,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, GetRequest, SessionService};
use async_trait::async_trait;
use serde_json::{Value, json};

use super::worker::{ParkReason, WorkerHooks};

/// The code `Runner::steer` answers a run that has not built its context yet, or has
/// finished, with.
const STEER_RUN_NOT_READY_CODE: &str = "agent.steer.run_not_ready";

/// How a [`DurableSubagents`] host runs sub-agents.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_core::RunBudget;
/// use adk_core::subagent::SubagentTarget;
/// use adk_server::background::SubagentConfig;
///
/// let config = SubagentConfig::default()
///     .with_queue("background")
///     .with_target(SubagentTarget::new("researcher", "Reads sources and cites them"))
///     .with_max_depth(1)
///     .with_default_budget(RunBudget::new().max_model_calls(10).max_wall_time(Duration::from_secs(600)));
/// assert_eq!(config.max_active, 4);
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SubagentConfig {
    /// The queue sub-agent runs and parent notices go to; a worker must claim it.
    /// Defaults to `default`.
    pub queue: String,
    /// The target a start that names none runs; the parent's own target when `None`.
    pub default_target: Option<String>,
    /// The targets a parent may name. Empty allows only the default target, or the
    /// parent's own.
    pub targets: Vec<SubagentTarget>,
    /// The budget a start that asks for none gets, before it is carved from the parent's.
    /// Defaults to 50 model calls, 200 tool calls and 30 minutes per attempt.
    pub default_budget: RunBudget,
    /// The largest budget a start may ask for: every ask is narrowed by it.
    pub max_budget: RunBudget,
    /// How deep a chain of sub-agents may go: 1 lets only top-level agents start them.
    /// Defaults to 2.
    pub max_depth: u32,
    /// How many unfinished sub-agents one parent session may have. Defaults to 4.
    pub max_active: usize,
    /// The most characters of a sub-agent's answer a notice carries. Defaults to 4,000.
    pub notice_chars: usize,
    /// The attempts each run may take, counting recoveries. Defaults to 3.
    pub max_attempts: u32,
    /// How far back [`DurableSubagents::reconcile`] looks for finished runs whose parent
    /// may not have been told. Defaults to 7 days.
    pub reconcile_window: std::time::Duration,
}

impl Default for SubagentConfig {
    fn default() -> Self {
        Self {
            queue: "default".to_string(),
            default_target: None,
            targets: Vec::new(),
            default_budget: RunBudget::new()
                .max_model_calls(50)
                .max_tool_calls(200)
                .max_wall_time(std::time::Duration::from_secs(30 * 60)),
            max_budget: RunBudget::new(),
            max_depth: 2,
            max_active: 4,
            notice_chars: 4_000,
            max_attempts: NewRun::DEFAULT_MAX_ATTEMPTS,
            reconcile_window: std::time::Duration::from_secs(7 * 24 * 60 * 60),
        }
    }
}

impl SubagentConfig {
    /// Sets the queue.
    #[must_use]
    pub fn with_queue(mut self, queue: impl Into<String>) -> Self {
        self.queue = queue.into();
        self
    }

    /// Sets the target a start that names none runs.
    #[must_use]
    pub fn with_default_target(mut self, target: impl Into<String>) -> Self {
        self.default_target = Some(target.into());
        self
    }

    /// Adds a target a parent may name.
    #[must_use]
    pub fn with_target(mut self, target: SubagentTarget) -> Self {
        self.targets.push(target);
        self
    }

    /// Sets the budget a start that asks for none gets.
    #[must_use]
    pub fn with_default_budget(mut self, budget: RunBudget) -> Self {
        self.default_budget = budget;
        self
    }

    /// Sets the largest budget a start may ask for.
    #[must_use]
    pub fn with_max_budget(mut self, budget: RunBudget) -> Self {
        self.max_budget = budget;
        self
    }

    /// Sets how deep a chain of sub-agents may go.
    #[must_use]
    pub fn with_max_depth(mut self, depth: u32) -> Self {
        self.max_depth = depth;
        self
    }

    /// Sets how many unfinished sub-agents one parent session may have.
    #[must_use]
    pub fn with_max_active(mut self, max: usize) -> Self {
        self.max_active = max;
        self
    }

    /// Sets the most characters of an answer a notice carries.
    #[must_use]
    pub fn with_notice_chars(mut self, chars: usize) -> Self {
        self.notice_chars = chars;
        self
    }
}

/// Where a [`DurableSubagents`] host enqueues, steers and cancels runs, and reports status:
/// straight to the run store and the runners ([`RunStoreBackend`]), or through a kernel's
/// process table, which records each sub-agent as a process (`adk-kernel`).
#[async_trait]
pub trait SubagentBackend: Send + Sync {
    /// Enqueues a run of sub-agent `launch`: its first, or a follow-up.
    ///
    /// # Errors
    ///
    /// The store's error, or the backend's refusal (a freeze, a quota).
    async fn enqueue_subagent(&self, run: NewRun, launch: &SubagentLaunch) -> Result<RunRecord>;

    /// Enqueues the run of the parent's session that tells it about `child`, a run of
    /// sub-agent `launch`.
    ///
    /// # Errors
    ///
    /// The store's error, or the backend's refusal.
    async fn enqueue_notice(
        &self,
        run: NewRun,
        child: &RunRecord,
        launch: &SubagentLaunch,
    ) -> Result<RunRecord>;

    /// Queues `text` on the steering inbox of the turn of session `session_id` that runs
    /// here, or `None` when none does.
    ///
    /// # Errors
    ///
    /// The steering path's error: an input guardrail or plugin refused the message.
    async fn steer(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        text: &str,
    ) -> Result<Option<SteerReceipt>>;

    /// Cancels run `run_id`: at once when it is queued or parked, at its next checkpoint
    /// when it runs.
    ///
    /// # Errors
    ///
    /// The store's error.
    async fn cancel(&self, run_id: &str) -> Result<()>;

    /// Reports that a sub-agent's status changed. The default does nothing.
    async fn changed(&self, _record: &SubagentRecord) {}
}

/// The [`SubagentBackend`] of a host without a kernel: runs go straight to the run store,
/// and steering to the runners it was given.
///
/// It holds its runners weakly and clones share them, so a runner whose agent carries the
/// sub-agent tools can be added after the host it reaches is built
/// ([`add_runner`](Self::add_runner)).
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_core::InMemoryRunStore;
/// use adk_server::background::RunStoreBackend;
///
/// let backend = RunStoreBackend::new(Arc::new(InMemoryRunStore::new()));
/// // Build the host over `Arc::new(backend.clone())`, the agents over the host, then:
/// // backend.add_runner(&runner);
/// # let _ = backend;
/// ```
#[derive(Clone)]
pub struct RunStoreBackend {
    runs: Arc<dyn DurableRunStore>,
    runners: Arc<std::sync::RwLock<Vec<std::sync::Weak<Runner>>>>,
}

impl std::fmt::Debug for RunStoreBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunStoreBackend").field("runners", &self.runners().len()).finish()
    }
}

impl RunStoreBackend {
    /// A backend over `runs` that steers no turn.
    pub fn new(runs: Arc<dyn DurableRunStore>) -> Self {
        Self { runs, runners: Arc::default() }
    }

    /// Steers the turns `runner` runs: the parents' and the sub-agents'.
    #[must_use]
    pub fn with_runner(self, runner: &Arc<Runner>) -> Self {
        self.add_runner(runner);
        self
    }

    /// Steers the turns `runner` runs, from now on; every clone of this backend sees it.
    pub fn add_runner(&self, runner: &Arc<Runner>) {
        self.runners.write().unwrap_or_else(|e| e.into_inner()).push(Arc::downgrade(runner));
    }

    fn runners(&self) -> Vec<Arc<Runner>> {
        self.runners
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter_map(std::sync::Weak::upgrade)
            .collect()
    }
}

#[async_trait]
impl SubagentBackend for RunStoreBackend {
    async fn enqueue_subagent(&self, run: NewRun, _launch: &SubagentLaunch) -> Result<RunRecord> {
        enqueued(self.runs.enqueue(run).await?)
    }

    async fn enqueue_notice(
        &self,
        run: NewRun,
        _child: &RunRecord,
        _launch: &SubagentLaunch,
    ) -> Result<RunRecord> {
        enqueued(self.runs.enqueue(run).await?)
    }

    async fn steer(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
        text: &str,
    ) -> Result<Option<SteerReceipt>> {
        let identity = AdkIdentity::new(
            AppName::try_from(app_name)?,
            UserId::try_from(user_id)?,
            SessionId::try_from(session_id)?,
        );
        for runner in self.runners().iter().filter(|runner| runner.app_name() == app_name) {
            match runner.steer(&identity, Content::new("user").with_text(text)).await {
                Ok(receipt) => return Ok(Some(receipt)),
                Err(error)
                    if error.code == STEER_NO_ACTIVE_RUN_CODE
                        || error.code == STEER_RUN_NOT_READY_CODE => {}
                Err(error) => return Err(error),
            }
        }
        Ok(None)
    }

    async fn cancel(&self, run_id: &str) -> Result<()> {
        self.runs.request_cancel(run_id).await
    }
}

/// The record of a created or existing run; a rejected one is an error.
fn enqueued(enqueued: Enqueued) -> Result<RunRecord> {
    match enqueued {
        Enqueued::Created(run) | Enqueued::Existing(run) => Ok(run),
        Enqueued::Rejected { blocking_run_id } => Err(AdkError::new(
            ErrorComponent::Server,
            ErrorCategory::Unavailable,
            "subagent.rejected",
            format!(
                "run {blocking_run_id} holds the run's concurrency key; try again once it ends"
            ),
        )),
    }
}

/// Background sub-agents as durable runs (see the [module docs](self)).
///
/// Cheap to clone: clones share one host.
#[derive(Clone)]
pub struct DurableSubagents {
    inner: Arc<Inner>,
}

/// A [`DurableSubagents`] host that is not kept alive, from [`DurableSubagents::downgrade`].
#[derive(Clone, Default)]
pub struct WeakSubagents(std::sync::Weak<Inner>);

impl std::fmt::Debug for WeakSubagents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WeakSubagents").field("alive", &(self.0.strong_count() > 0)).finish()
    }
}

impl WeakSubagents {
    /// The host, while it lives.
    pub fn upgrade(&self) -> Option<DurableSubagents> {
        self.0.upgrade().map(|inner| DurableSubagents { inner })
    }
}

struct Inner {
    runs: Arc<dyn DurableRunStore>,
    sessions: Arc<dyn SessionService>,
    backend: Arc<dyn SubagentBackend>,
    config: SubagentConfig,
    /// Sub-agents their parent cancelled: their cancellation tells the parent nothing.
    cancelled: Mutex<HashSet<String>>,
}

impl std::fmt::Debug for DurableSubagents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DurableSubagents")
            .field("config", &self.inner.config)
            .finish_non_exhaustive()
    }
}

impl DurableSubagents {
    /// A host whose sub-agents run on `runs`, in sessions of `sessions`, through `backend`.
    pub fn new(
        runs: Arc<dyn DurableRunStore>,
        sessions: Arc<dyn SessionService>,
        backend: Arc<dyn SubagentBackend>,
        config: SubagentConfig,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                runs,
                sessions,
                backend,
                config,
                cancelled: Mutex::new(HashSet::new()),
            }),
        }
    }

    /// The host's configuration.
    pub fn config(&self) -> &SubagentConfig {
        &self.inner.config
    }

    /// A handle on this host that does not keep it alive, for a tool built before the
    /// host's owner — a kernel — exists.
    pub fn downgrade(&self) -> WeakSubagents {
        WeakSubagents(Arc::downgrade(&self.inner))
    }

    /// The runs of `app_name`'s `user_id` on the host's queue that belong to a sub-agent,
    /// grouped by sub-agent id, each group oldest first.
    async fn groups(
        &self,
        filter: RunFilter,
    ) -> Result<BTreeMap<String, (SubagentLaunch, Vec<RunRecord>)>> {
        let mut groups: BTreeMap<String, (SubagentLaunch, Vec<RunRecord>)> = BTreeMap::new();
        for run in self.inner.runs.list(filter.queue(self.inner.config.queue.as_str())).await? {
            let Some(launch) = SubagentLaunch::of_run(&run) else { continue };
            // A launch is a claim in the run's input: it counts only for its own user.
            if launch.parent.app_name != run.app_name || launch.parent.user_id != run.user_id {
                continue;
            }
            groups.entry(launch.id.clone()).or_insert_with(|| (launch, Vec::new())).1.push(run);
        }
        for (launch, runs) in groups.values_mut() {
            runs.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.run_id.cmp(&b.run_id)));
            // The first run's launch is the sub-agent's.
            if let Some(first) = runs.first().and_then(SubagentLaunch::of_run) {
                *launch = first;
            }
        }
        Ok(groups)
    }

    /// The runs of sub-agent `id` with its launch, when a parent in `scope` started it.
    async fn subagent(
        &self,
        scope: &SubagentScope,
        id: &str,
    ) -> Result<Option<(SubagentLaunch, Vec<RunRecord>)>> {
        let filter = RunFilter::default()
            .app_name(scope.app_name.as_str())
            .user_id(scope.user_id.as_str())
            .concurrency_key(format!("subagent:{id}"));
        Ok(self
            .groups(filter)
            .await?
            .remove(id)
            .filter(|(launch, _)| scope.contains(&launch.parent)))
    }

    /// The sub-agent `id` of any parent, for the host itself.
    async fn subagent_any(
        &self,
        app_name: &str,
        user_id: &str,
        id: &str,
    ) -> Result<Option<(SubagentLaunch, Vec<RunRecord>)>> {
        let filter = RunFilter::default()
            .app_name(app_name)
            .user_id(user_id)
            .concurrency_key(format!("subagent:{id}"));
        Ok(self.groups(filter).await?.remove(id))
    }

    /// The target a start runs, after the host's rules.
    fn target_for(&self, start: &SubagentStart, parent_target: Option<&str>) -> Result<String> {
        let config = &self.inner.config;
        let asked = start.request.target.as_deref().map(str::trim).filter(|t| !t.is_empty());
        let target =
            asked.or(config.default_target.as_deref()).or(parent_target).ok_or_else(|| {
                subagent_refused(
                    "this host has no default sub-agent target and the parent's target is \
                     unknown; name a target",
                )
            })?;
        let allowed = config.targets.iter().any(|known| known.name == target)
            || config.default_target.as_deref() == Some(target)
            || (config.targets.is_empty() && parent_target == Some(target));
        if !allowed {
            let mut names: Vec<&str> = config.targets.iter().map(|t| t.name.as_str()).collect();
            names.extend(config.default_target.as_deref());
            return Err(subagent_refused(format!(
                "'{target}' is not a sub-agent target of this host; choose one of: {}",
                if names.is_empty() {
                    "the parent's own agent".to_string()
                } else {
                    names.join(", ")
                }
            )));
        }
        Ok(target.to_string())
    }

    /// Copies the parent's session into the sub-agent's: its state, and its events but the
    /// ones holding calls still waiting for a response, each under a `fork-` invocation id.
    async fn fork(&self, parent: &SubagentParent, session_id: &str) -> Result<()> {
        let sessions = &self.inner.sessions;
        let existing = sessions
            .get(GetRequest {
                app_name: parent.app_name.clone(),
                user_id: parent.user_id.clone(),
                session_id: session_id.to_string(),
                num_recent_events: None,
                after: None,
            })
            .await;
        // A replayed start finds the fork it already made.
        if existing.is_ok() {
            return Ok(());
        }
        let source = sessions
            .get(GetRequest {
                app_name: parent.app_name.clone(),
                user_id: parent.user_id.clone(),
                session_id: parent.session_id.clone(),
                num_recent_events: None,
                after: None,
            })
            .await?;
        let events = source.events().all();
        let state: HashMap<String, Value> =
            source.state().all().into_iter().filter(|(key, _)| !key.contains(':')).collect();
        let answered: HashSet<String> = events
            .iter()
            .flat_map(|event| {
                event
                    .tool_results()
                    .into_iter()
                    .filter_map(|result| result.call_id.map(str::to_string))
            })
            .collect();
        sessions
            .create(CreateRequest {
                app_name: parent.app_name.clone(),
                user_id: parent.user_id.clone(),
                session_id: Some(session_id.to_string()),
                state,
            })
            .await?;
        for event in events {
            if event.llm_response.partial
                || event
                    .tool_calls()
                    .iter()
                    .any(|call| call.call_id.is_none_or(|id| !answered.contains(id)))
            {
                continue;
            }
            let mut copy = event.clone();
            copy.id = uuid::Uuid::new_v4().to_string();
            copy.invocation_id = format!("{FORK_INVOCATION_PREFIX}{}", event.invocation_id);
            sessions.append_event(session_id, copy).await?;
        }
        Ok(())
    }

    /// A follow-up run of sub-agent `launch` with `text` as its user turn: the first run's
    /// input with the text replaced, so it keeps the slice and the ceiling.
    fn follow_up_run(&self, launch: &SubagentLaunch, first: &RunRecord, text: &str) -> NewRun {
        let mut input = first.input.clone();
        if let Some(map) = input.as_object_mut() {
            map.insert("text".to_string(), Value::String(text.to_string()));
        }
        let mut run = NewRun::new(
            self.inner.config.queue.as_str(),
            first.app_name.as_str(),
            first.user_id.as_str(),
            first.target.as_str(),
            input,
        )
        .with_session_id(launch.session_id())
        .with_concurrency_key(launch.concurrency_key(), OnConflict::Enqueue)
        .with_max_attempts(self.inner.config.max_attempts);
        run.principal.clone_from(&first.principal);
        run
    }

    /// Queues `text` for sub-agent `launch`: on its running turn's inbox, or as its next run.
    async fn deliver_to_subagent(
        &self,
        launch: &SubagentLaunch,
        runs: &[RunRecord],
        text: &str,
        key: Option<String>,
    ) -> Result<FollowUp> {
        let (Some(first), Some(latest)) = (runs.first(), runs.last()) else {
            return Err(subagent_not_found(&launch.id));
        };
        if latest.state == RunState::Running
            && let Some(receipt) = self
                .inner
                .backend
                .steer(&first.app_name, &first.user_id, &launch.session_id(), text)
                .await?
        {
            let steer_id = receipt.steer_id().to_string();
            let host = self.clone();
            let launch = launch.clone();
            let first = first.clone();
            let text = text.to_string();
            tokio::spawn(async move {
                if let Ok(SteerOutcome::Returned(_)) = receipt.outcome().await {
                    let mut run = host.follow_up_run(&launch, &first, &text);
                    run.idempotency_key = key;
                    if let Err(error) = host.inner.backend.enqueue_subagent(run, &launch).await {
                        tracing::warn!(subagent.id = %launch.id, error = %error, "could not queue the follow-up the running turn returned");
                    }
                }
            });
            return Ok(FollowUp::Steered { steer_id, run_id: latest.run_id.clone() });
        }
        let mut run = self.follow_up_run(launch, first, text);
        run.idempotency_key = key;
        let run = self.inner.backend.enqueue_subagent(run, launch).await?;
        Ok(FollowUp::Queued { run_id: run.run_id })
    }

    /// Reports a status change of the sub-agent `run` belongs to and returns its record.
    async fn report(
        &self,
        run: &RunRecord,
    ) -> Option<(SubagentLaunch, Vec<RunRecord>, SubagentRecord)> {
        let launch = SubagentLaunch::of_run(run)?;
        if launch.parent.app_name != run.app_name || launch.parent.user_id != run.user_id {
            tracing::warn!(run.id = %run.run_id, "a run names a sub-agent parent of another user; ignoring it");
            return None;
        }
        let (launch, mut runs) = match self
            .subagent_any(&run.app_name, &run.user_id, &launch.id)
            .await
        {
            Ok(Some(found)) => found,
            Ok(None) => (launch, vec![run.clone()]),
            Err(error) => {
                tracing::warn!(run.id = %run.run_id, error = %error, "could not read the sub-agent's runs");
                (launch, vec![run.clone()])
            }
        };
        // The store may not show the write the hook reports yet.
        if let Some(stored) = runs.iter_mut().find(|stored| stored.run_id == run.run_id) {
            *stored = run.clone();
        }
        let latest = runs.iter().position(|stored| stored.run_id == run.run_id)?;
        let record = SubagentRecord::from_runs(&launch, &runs[..=latest])?;
        self.inner.backend.changed(&record).await;
        Some((launch, runs, record))
    }

    /// Tells sub-agent `run`'s parent that the run finished, when it should be told; call
    /// it after the store recorded the run's outcome. [`SubagentHooks::on_finish`] does.
    pub async fn run_finished(&self, run: &RunRecord) {
        if !run.state.is_terminal() {
            return;
        }
        let Some((launch, _, record)) = self.report(run).await else { return };
        if record.status == SubagentStatus::Cancelled
            && self.inner.cancelled.lock().unwrap_or_else(|e| e.into_inner()).remove(&launch.id)
        {
            return;
        }
        if !launch.notify_parent {
            return;
        }
        if let Err(error) = self.tell_parent(&launch, run, &record, true, true).await {
            tracing::warn!(subagent.id = %launch.id, run.id = %run.run_id, error = %error, "could not tell the parent its sub-agent finished; it is told when the host starts again");
        }
    }

    /// Reports a sub-agent `run` parked, and tells a parent turn in flight that it waits
    /// for a person. [`SubagentHooks::on_park`] calls it.
    pub async fn run_parked(&self, run: &RunRecord) {
        let Some((launch, _, record)) = self.report(run).await else { return };
        if record.status != SubagentStatus::WaitingForHuman || !launch.notify_parent {
            return;
        }
        if let Err(error) = self.tell_parent(&launch, run, &record, false, true).await {
            tracing::debug!(subagent.id = %launch.id, error = %error, "could not tell the parent its sub-agent waits for a person");
        }
    }

    /// Reports sub-agent `run` started running. [`SubagentHooks::on_claim`] calls it.
    pub async fn run_claimed(&self, run: &RunRecord) {
        let _ = self.report(run).await;
    }

    /// Tells the parent of `launch` about `record`: by steering its turn in flight, else,
    /// when `durable`, by a run of its session (a follow-up when the parent is itself a
    /// sub-agent), keyed so it is told once.
    async fn tell_parent(
        &self,
        launch: &SubagentLaunch,
        child: &RunRecord,
        record: &SubagentRecord,
        durable: bool,
        steer: bool,
    ) -> Result<()> {
        let notice = subagent_notice(record, self.inner.config.notice_chars);
        let header = notice_header(&record.id, &record.name, &record.run_id, record.status);
        let parent = &launch.parent;
        if durable && self.parent_was_told(parent, &header).await {
            return Ok(());
        }
        let key = format!("subagent:{}:{}:{}", record.id, record.run_id, record.status.as_str());
        if let Some(parent_id) = &launch.parent_subagent {
            let Some((parent_launch, runs)) =
                self.subagent_any(&parent.app_name, &parent.user_id, parent_id).await?
            else {
                return Ok(());
            };
            if !durable {
                if steer && runs.last().is_some_and(|run| run.state == RunState::Running) {
                    let _ = self
                        .inner
                        .backend
                        .steer(
                            &parent.app_name,
                            &parent.user_id,
                            &parent_launch.session_id(),
                            &notice,
                        )
                        .await?;
                }
                return Ok(());
            }
            if steer {
                self.deliver_to_subagent(&parent_launch, &runs, &notice, Some(key)).await?;
            } else if let Some(first) = runs.first() {
                let mut run = self.follow_up_run(&parent_launch, first, &notice);
                run.idempotency_key = Some(key);
                self.inner.backend.enqueue_subagent(run, &parent_launch).await?;
            }
            return Ok(());
        }
        let receipt = if steer {
            self.inner
                .backend
                .steer(&parent.app_name, &parent.user_id, &parent.session_id, &notice)
                .await?
        } else {
            None
        };
        if let Some(receipt) = receipt {
            tracing::info!(subagent.id = %record.id, steer.id = receipt.steer_id(), status = %record.status, "told the parent's running turn");
            if durable {
                let host = self.clone();
                let launch = launch.clone();
                let child = child.clone();
                tokio::spawn(async move {
                    if let Ok(SteerOutcome::Returned(_)) = receipt.outcome().await
                        && let Err(error) =
                            host.enqueue_parent_turn(&launch, &child, &notice, key).await
                    {
                        tracing::warn!(subagent.id = %launch.id, error = %error, "could not wake the parent after its turn ended");
                    }
                });
            }
            return Ok(());
        }
        if durable {
            self.enqueue_parent_turn(launch, child, &notice, key).await?;
        }
        Ok(())
    }

    /// Whether the parent's session already holds the notice that starts with `header`.
    async fn parent_was_told(&self, parent: &SubagentParent, header: &str) -> bool {
        let Ok(session) = self
            .inner
            .sessions
            .get(GetRequest {
                app_name: parent.app_name.clone(),
                user_id: parent.user_id.clone(),
                session_id: parent.session_id.clone(),
                num_recent_events: None,
                after: None,
            })
            .await
        else {
            return false;
        };
        session.events().all().iter().any(|event| {
            event.author == "user"
                && event.llm_response.content.as_ref().is_some_and(|content| {
                    content.parts.iter().filter_map(Part::text).any(|text| text.starts_with(header))
                })
        })
    }

    /// Enqueues the parent's turn that receives `notice`.
    async fn enqueue_parent_turn(
        &self,
        launch: &SubagentLaunch,
        child: &RunRecord,
        notice: &str,
        key: String,
    ) -> Result<()> {
        let parent = &launch.parent;
        let Some(target) = parent.target.as_deref() else {
            tracing::info!(subagent.id = %launch.id, "the parent's target is unknown, so only a turn in flight is told; the parent polls");
            return Ok(());
        };
        let mut run = NewRun::new(
            self.inner.config.queue.as_str(),
            parent.app_name.as_str(),
            parent.user_id.as_str(),
            target,
            json!({ "text": notice }),
        )
        .with_session_id(parent.session_id.as_str())
        .with_idempotency_key(key)
        // Notices for one session run one at a time, so two sub-agents finishing together
        // never run two turns of the parent's session at once.
        .with_concurrency_key(format!("subagent-notice:{}", parent.session_id), OnConflict::Enqueue)
        .with_max_attempts(self.inner.config.max_attempts);
        if let Some(acting) = &parent.acting {
            let mut principal = adk_core::ActingContext::new(adk_core::AgentPrincipal::derived(
                &parent.app_name,
                target,
            ));
            principal.on_behalf_of.clone_from(&acting.on_behalf_of);
            principal.delegation_chain.clone_from(&acting.delegation_chain);
            run.principal = Some(principal);
        }
        let woken = self.inner.backend.enqueue_notice(run, child, launch).await?;
        tracing::info!(subagent.id = %launch.id, parent.session = %parent.session_id, run.id = %woken.run_id, "woke the parent with its sub-agent's notice");
        Ok(())
    }

    /// Tells every parent about finished runs of its sub-agents it was not told about: a
    /// host that stopped between a run's end and the notice — a crash, `kill -9` — catches
    /// up here. It looks at runs that finished within
    /// [`SubagentConfig::reconcile_window`], leaves cancelled runs out, and tells a parent by
    /// a keyed run of its session only, never by steering a turn, so a notice already
    /// queued is not delivered twice. Returns how many finished runs it looked at.
    ///
    /// # Errors
    ///
    /// The store's error.
    pub async fn reconcile(&self) -> Result<usize> {
        let groups = self.groups(RunFilter::default()).await?;
        let since = chrono::Duration::from_std(self.inner.config.reconcile_window)
            .ok()
            .and_then(|window| chrono::Utc::now().checked_sub_signed(window))
            .unwrap_or(chrono::DateTime::<chrono::Utc>::MIN_UTC);
        let mut looked = 0;
        for (launch, runs) in groups.values() {
            if !launch.notify_parent {
                continue;
            }
            for (index, run) in runs.iter().enumerate() {
                if !matches!(run.state, RunState::Completed | RunState::Failed)
                    || run.updated_at < since
                {
                    continue;
                }
                looked += 1;
                let Some(record) = SubagentRecord::from_runs(launch, &runs[..=index]) else {
                    continue;
                };
                if let Err(error) = self.tell_parent(launch, run, &record, true, false).await {
                    tracing::warn!(subagent.id = %launch.id, run.id = %run.run_id, error = %error, "could not tell the parent on reconcile");
                }
            }
        }
        Ok(looked)
    }
}

#[async_trait]
impl SubagentHost for DurableSubagents {
    async fn start(&self, start: SubagentStart) -> Result<SubagentRecord> {
        let config = &self.inner.config;
        let mut parent = start.parent.clone();
        let scope = parent.scope();
        // The parent's own run says how deep it is and which target its session runs.
        let parent_run = match &parent.run_id {
            Some(run_id) => self.inner.runs.get(run_id).await?,
            None => None,
        };
        let parent_launch = parent_run.as_ref().and_then(SubagentLaunch::of_run);
        if parent.target.is_none() {
            parent.target = parent_run.as_ref().map(|run| run.target.clone());
        }
        let depth = parent_launch.as_ref().map_or(1, |launch| launch.depth + 1);
        if depth > config.max_depth {
            return Err(subagent_refused(format!(
                "a sub-agent at depth {depth} would exceed this host's limit of {}; do the work \
                 in this agent instead",
                config.max_depth
            )));
        }
        let mine = self
            .groups(
                RunFilter::default()
                    .app_name(scope.app_name.as_str())
                    .user_id(scope.user_id.as_str()),
            )
            .await?;
        let mine: Vec<&(SubagentLaunch, Vec<RunRecord>)> =
            mine.values().filter(|(launch, _)| scope.contains(&launch.parent)).collect();
        if let Some(key) = &start.idempotency_key
            && let Some((launch, runs)) =
                mine.iter().find(|(launch, _)| launch.start_key.as_deref() == Some(key))
        {
            return SubagentRecord::from_runs(launch, runs)
                .ok_or_else(|| subagent_not_found(&launch.id));
        }
        let active = mine
            .iter()
            .filter(|(_, runs)| runs.last().is_some_and(|run| !run.state.is_terminal()))
            .count();
        if active >= config.max_active {
            return Err(subagent_refused(format!(
                "this session already has {active} unfinished sub-agents, the most this host \
                 allows ({}); wait for one to finish or cancel one",
                config.max_active
            )));
        }
        let target = self.target_for(&start, parent.target.as_deref())?;
        // The limits a start asks for replace the default's; the rest keep the default's.
        let mut ask = config.default_budget.clone();
        if let Some(asked) = &start.request.budget {
            ask.max_model_calls = asked.max_model_calls.or(ask.max_model_calls);
            ask.max_total_tokens = asked.max_total_tokens.or(ask.max_total_tokens);
            ask.max_cost_micro_usd = asked.max_cost_micro_usd.or(ask.max_cost_micro_usd);
            ask.max_wall_time = asked.max_wall_time.or(ask.max_wall_time);
            ask.max_tool_calls = asked.max_tool_calls.or(ask.max_tool_calls);
            ask.allow_unpriced_models |= asked.allow_unpriced_models;
        }
        let ask = ask.narrowed(&config.max_budget);
        let budget = match &start.tracker {
            Some(tracker) => tracker.carve(&ask).map_err(AdkError::from)?,
            None => ask,
        };
        let id = format!("sa-{}", uuid::Uuid::new_v4().simple());
        let name = start
            .request
            .name
            .clone()
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| id.chars().take(11).collect());
        let mut launch =
            SubagentLaunch::new(&id, name, &target, parent.clone(), &start.request.task);
        launch.mode = start.request.mode;
        launch.depth = depth;
        launch.notify_parent = start.request.notify_parent;
        launch.parent_subagent = parent_launch.map(|launch| launch.id);
        launch.start_key.clone_from(&start.idempotency_key);
        if launch.mode == adk_core::SubagentMode::Fork {
            self.fork(&parent, &launch.session_id()).await?;
        }
        let mut limits = RunLimits::new().with_budget(budget);
        if let Some(tools) = &start.allowed_tools {
            limits = limits.with_allowed_tools(tools.iter().cloned());
        }
        let input = json!({
            "text": start.request.task,
            SUBAGENT_INPUT_KEY: launch,
            RUN_LIMITS_INPUT_KEY: limits,
        });
        let mut run = NewRun::new(
            config.queue.as_str(),
            parent.app_name.as_str(),
            parent.user_id.as_str(),
            target.as_str(),
            input,
        )
        .with_session_id(launch.session_id())
        .with_concurrency_key(launch.concurrency_key(), OnConflict::Enqueue)
        .with_max_attempts(config.max_attempts)
        .with_principal(parent.child_acting(&target));
        if let Some(key) = &start.idempotency_key {
            run = run.with_idempotency_key(format!("subagent:start:{key}"));
        }
        let record = self.inner.backend.enqueue_subagent(run, &launch).await?;
        let launch = SubagentLaunch::of_run(&record).unwrap_or(launch);
        let record = SubagentRecord::from_runs(&launch, std::slice::from_ref(&record))
            .ok_or_else(|| subagent_not_found(&launch.id))?;
        tracing::info!(subagent.id = %record.id, subagent.target = %record.target, subagent.depth = record.depth, run.id = %record.run_id, "sub-agent started");
        self.inner.backend.changed(&record).await;
        Ok(record)
    }

    async fn get(&self, scope: &SubagentScope, id: &str) -> Result<Option<SubagentRecord>> {
        Ok(self
            .subagent(scope, id)
            .await?
            .and_then(|(launch, runs)| SubagentRecord::from_runs(&launch, &runs)))
    }

    async fn list(&self, scope: &SubagentScope) -> Result<Vec<SubagentRecord>> {
        let filter =
            RunFilter::default().app_name(scope.app_name.as_str()).user_id(scope.user_id.as_str());
        let mut records: Vec<SubagentRecord> = self
            .groups(filter)
            .await?
            .values()
            .filter(|(launch, _)| scope.contains(&launch.parent))
            .filter_map(|(launch, runs)| SubagentRecord::from_runs(launch, runs))
            .collect();
        records.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        Ok(records)
    }

    async fn send(&self, scope: &SubagentScope, id: &str, message: &str) -> Result<FollowUp> {
        if message.trim().is_empty() {
            return Err(AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::InvalidInput,
                adk_core::STEER_INVALID_MESSAGE_CODE,
                "the follow-up message is empty; send the text the sub-agent should act on",
            ));
        }
        let Some((launch, runs)) = self.subagent(scope, id).await? else {
            return Err(subagent_not_found(id));
        };
        if runs.last().is_some_and(|run| run.state == RunState::Cancelled) {
            return Err(AdkError::new(
                ErrorComponent::Agent,
                ErrorCategory::InvalidInput,
                SUBAGENT_FINISHED_CODE,
                format!("sub-agent '{id}' was cancelled and takes no more work; start a new one"),
            ));
        }
        let follow_up = self.deliver_to_subagent(&launch, &runs, message, None).await?;
        tracing::info!(subagent.id = %id, follow_up = ?follow_up, "follow-up sent to sub-agent");
        Ok(follow_up)
    }

    async fn cancel(&self, scope: &SubagentScope, id: &str) -> Result<SubagentRecord> {
        let Some((launch, runs)) = self.subagent(scope, id).await? else {
            return Err(subagent_not_found(id));
        };
        let unfinished: Vec<&RunRecord> =
            runs.iter().filter(|run| !run.state.is_terminal()).collect();
        if !unfinished.is_empty() {
            self.inner
                .cancelled
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(launch.id.clone());
        }
        for run in unfinished {
            self.inner.backend.cancel(&run.run_id).await?;
        }
        tracing::info!(subagent.id = %id, "sub-agent cancelled by its parent");
        let (launch, runs) = self.subagent(scope, id).await?.unwrap_or((launch, runs));
        let record =
            SubagentRecord::from_runs(&launch, &runs).ok_or_else(|| subagent_not_found(id))?;
        // A queued or parked run is cancelled at once and no worker reports it.
        if record.status.is_terminal() {
            self.inner.cancelled.lock().unwrap_or_else(|e| e.into_inner()).remove(&launch.id);
            self.inner.backend.changed(&record).await;
        }
        Ok(record)
    }

    fn targets(&self) -> Vec<SubagentTarget> {
        self.inner.config.targets.clone()
    }
}

/// The [`WorkerHooks`] that report a plain [`DurableWorker`](super::DurableWorker)'s
/// sub-agent runs to a [`DurableSubagents`] host: claims, parks on held calls, and
/// finished runs, whose parents it tells.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_server::background::{DurableSubagents, DurableWorker, SubagentHooks};
///
/// # fn example(worker: DurableWorker, subagents: DurableSubagents) {
/// let worker = worker.with_hooks(Arc::new(SubagentHooks::new(subagents)));
/// # let _ = worker;
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct SubagentHooks {
    subagents: DurableSubagents,
}

impl SubagentHooks {
    /// Hooks reporting to `subagents`.
    pub fn new(subagents: DurableSubagents) -> Self {
        Self { subagents }
    }
}

#[async_trait]
impl WorkerHooks for SubagentHooks {
    async fn on_claim(&self, run: &RunRecord) {
        if run.input.get(SUBAGENT_INPUT_KEY).is_some() {
            self.subagents.run_claimed(run).await;
        }
    }

    async fn on_park(&self, run: &RunRecord, reason: ParkReason) {
        if reason == ParkReason::AwaitingApproval && run.input.get(SUBAGENT_INPUT_KEY).is_some() {
            self.subagents.run_parked(run).await;
        }
    }

    async fn on_finish(&self, run: &RunRecord) {
        if run.input.get(SUBAGENT_INPUT_KEY).is_some() {
            self.subagents.run_finished(run).await;
        }
    }
}
