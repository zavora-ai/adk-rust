//! # Background Runs and Cron Scheduling
//!
//! REST endpoints and runtime for durable background runs and schedules, gated behind
//! the `background` feature.
//!
//! Runs live in a [`DurableRunStore`] and schedules in a [`ScheduleStore`](adk_core::ScheduleStore) — in memory by
//! default, or a shared backend so several replicas serve one queue. A
//! [`DurableWorker`] claims runs under fenced leases and executes them; a
//! [`DurableScheduler`] on every replica enqueues each schedule's fire times exactly
//! once. Both respect the governance kill switch.
//!
//! ## Background runs
//!
//! - `POST /runs` — Enqueue a run of a workflow or an agent
//! - `GET /runs` — List runs (`state`, `sessionId`, `limit` query parameters)
//! - `GET /runs/{run_id}` — Get a run
//! - `DELETE /runs/{run_id}` — Cancel a run; a running run stops at its worker's next heartbeat
//! - `POST /runs/{run_id}/wake` — Make a run parked on a held tool call claimable now
//!
//! ## Cron jobs
//!
//! - `POST /cron` — Create a cron schedule
//! - `GET /cron` — List schedules
//! - `GET /cron/{job_id}` — Get a schedule
//! - `PATCH /cron/{job_id}` — Pause or resume a schedule
//! - `DELETE /cron/{job_id}` — Delete a schedule
//!
//! ## Usage
//!
//! > **Important:** these routers carry no authentication of their own. Prefer
//! > [`ServerBuilder::with_background_runs`](crate::ServerBuilder::with_background_runs)
//! > and [`ServerBuilder::with_cron_jobs`](crate::ServerBuilder::with_cron_jobs), which
//! > mount them under `/api` behind the server's auth middleware. A standalone mount
//! > must sit behind an authentication layer, or anyone who reaches it can submit,
//! > schedule, and cancel work.
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use adk_server::background::{BackgroundState, CronState, WorkflowRegistry, start_cron_scheduler};
//!
//! // In memory, one process: the embedded worker starts with the server.
//! let background = BackgroundState::new().with_executor(Arc::new(registry));
//!
//! // Shared stores, several replicas: each replica runs a worker and a scheduler.
//! let background = BackgroundState::new()
//!     .with_run_store(run_store)
//!     .with_executor(Arc::new(registry))
//!     .with_agent("support", Arc::new(runner));
//! let cron = CronState::new(background.clone()).with_schedule_store(schedule_store);
//! start_cron_scheduler(cron.clone());
//! let app = ServerBuilder::new(config)
//!     .with_background_runs(background)
//!     .with_cron_jobs(cron)
//!     .build();
//! ```

pub mod cron;
pub mod scheduler;
pub mod worker;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use adk_core::{
    AdkError, DurableRunStore, Enqueued, GovernanceControl, InMemoryRunStore, NewRun, OnConflict,
    RequestContext, RunCheckpointRef, RunFilter, RunRecord, RunState,
};
use adk_runner::Runner;
use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

pub use cron::{
    ConcurrencyPolicy, CreateCronJobRequest, CronJobResponse, CronJobStatus, CronState,
    cron_jobs_router, cron_jobs_router_with_state, start_cron_scheduler, validate_cron_expression,
};
pub use scheduler::{DurableScheduler, SchedulerTick};
pub use worker::{DurableWorker, DurableWorkerConfig, WorkerHandle};

// ---------------------------------------------------------------------------
// Data Types
// ---------------------------------------------------------------------------

/// Workflow input state — a map of string keys to JSON values.
pub type WorkflowState = HashMap<String, Value>;

/// Run lifecycle status as the REST API reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum RunStatus {
    /// Waiting for a worker.
    Queued,
    /// Claimed by a worker.
    Running,
    /// Parked until a timer passes or the run is woken.
    Waiting,
    /// Finished with a result.
    Completed,
    /// Finished with an error.
    Failed,
    /// Cancelled before or during execution.
    Cancelled,
}

impl From<RunState> for RunStatus {
    fn from(state: RunState) -> Self {
        match state {
            RunState::Queued => Self::Queued,
            RunState::Running => Self::Running,
            RunState::Waiting => Self::Waiting,
            RunState::Completed => Self::Completed,
            RunState::Failed => Self::Failed,
            RunState::Cancelled => Self::Cancelled,
            _ => Self::Failed,
        }
    }
}

/// `POST /runs` request body.
///
/// # Example
///
/// ```rust
/// use adk_server::background::SubmitRunRequest;
///
/// let request: SubmitRunRequest = serde_json::from_value(serde_json::json!({
///     "target": "support",
///     "input": { "text": "where is order 7?" },
///     "sessionId": "thread-42",
///     "concurrencyKey": "thread-42",
///     "onConflict": "interrupt",
///     "idempotencyKey": "webhook:evt_1"
/// })).unwrap();
/// assert_eq!(request.target.as_deref(), Some("support"));
/// ```
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitRunRequest {
    /// The agent or workflow to run.
    #[serde(default)]
    pub target: Option<String>,
    /// The workflow to run; an alias of `target` kept for existing clients.
    #[serde(default)]
    pub workflow_id: Option<String>,
    /// The run's input: workflow state for a workflow, a string, `{"text": ...}`, or
    /// `{"content": ...}` for an agent.
    #[serde(default)]
    pub input: Value,
    /// Retries after the first attempt; `maxAttempts` is this plus one. Defaults to 0.
    #[serde(default)]
    pub max_retries: Option<u32>,
    /// The session an agent run continues; defaults to `run-{runId}`.
    #[serde(default)]
    pub session_id: Option<String>,
    /// The user the run belongs to, when the request is not authenticated.
    #[serde(default)]
    pub user_id: Option<String>,
    /// Enqueueing an existing key returns the existing run.
    #[serde(default)]
    pub idempotency_key: Option<String>,
    /// Runs sharing a key run one at a time.
    #[serde(default)]
    pub concurrency_key: Option<String>,
    /// What happens when an unfinished run holds `concurrencyKey` (double texting).
    #[serde(default)]
    pub on_conflict: Option<OnConflict>,
    /// The run is not claimed before this time.
    #[serde(default)]
    pub not_before: Option<DateTime<Utc>>,
}

/// `POST /runs` response.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitRunResponse {
    /// The run identifier.
    pub run_id: String,
    /// The run's status.
    pub status: RunStatus,
    /// When the run was enqueued.
    pub created_at: String,
    /// `true` when the idempotency key matched an existing run.
    pub existing: bool,
}

/// `GET /runs/{run_id}` response.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStatusResponse {
    /// The run identifier.
    pub run_id: String,
    /// The agent or workflow the run executes.
    pub target: String,
    /// The run's status.
    pub status: RunStatus,
    /// When the run was enqueued.
    pub created_at: String,
    /// When the run last changed.
    pub updated_at: String,
    /// The result of a completed run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// The error of the latest failed attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Attempts started so far, counting recoveries.
    pub attempt: u32,
    /// The most attempts the run may take.
    pub max_attempts: u32,
    /// Retries used: `attempt - 1`.
    pub retry_count: u32,
    /// Attempts left.
    pub retries_remaining: u32,
    /// Whether cancellation was requested while the run was running.
    pub cancel_requested: bool,
    /// The session an agent run continues.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Where the run's progress is checkpointed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<RunCheckpointRef>,
}

impl From<RunRecord> for RunStatusResponse {
    fn from(run: RunRecord) -> Self {
        Self {
            target: run.target,
            status: run.state.into(),
            created_at: run.created_at.to_rfc3339(),
            updated_at: run.updated_at.to_rfc3339(),
            result: run.result,
            error: run.error,
            retry_count: run.attempt.saturating_sub(1),
            retries_remaining: run.max_attempts.saturating_sub(run.attempt),
            attempt: run.attempt,
            max_attempts: run.max_attempts,
            cancel_requested: run.cancel_requested,
            session_id: run.session_id,
            checkpoint: run.checkpoint,
            run_id: run.run_id,
        }
    }
}

// ---------------------------------------------------------------------------
// Workflow executors
// ---------------------------------------------------------------------------

/// Executes a registered workflow for a background run.
///
/// A background run names a target; something has to turn that into work. Implement
/// this to bridge to whatever executes workflows in your application — `adk-graph`, the
/// functional API, or your own dispatcher.
///
/// # Errors
///
/// Return `Err` to fail the attempt. A failed attempt is retried while the run has
/// attempts left, so surface real failures rather than encoding them as `Ok`.
#[async_trait::async_trait]
pub trait WorkflowExecutor: Send + Sync {
    /// Whether `workflow_id` can be executed.
    ///
    /// Checked before a run is queued, so an unknown workflow is rejected instead of
    /// being accepted and then failed.
    fn has_workflow(&self, workflow_id: &str) -> bool;

    /// Run `workflow_id` with `input`.
    ///
    /// `cancel_token` fires when the run is cancelled; implementations should observe it
    /// and stop.
    async fn execute(
        &self,
        workflow_id: &str,
        input: WorkflowState,
        cancel_token: CancellationToken,
    ) -> std::result::Result<Value, String>;

    /// Run `workflow_id` as one attempt of a durable run.
    ///
    /// `run` carries the run's identity, its checkpoint thread, whether this attempt
    /// resumes a stopped one, and the cancellation token. A graph workflow passes
    /// `run.thread_id` to its executor so a resumed attempt continues from its
    /// checkpointer, and may report checkpoints with
    /// [`WorkflowRunContext::record_checkpoint`]. The default calls
    /// [`execute`](Self::execute), which restarts the workflow from its input.
    async fn execute_durable(
        &self,
        workflow_id: &str,
        input: WorkflowState,
        run: WorkflowRunContext,
    ) -> std::result::Result<Value, String> {
        self.execute(workflow_id, input, run.cancel).await
    }
}

/// Records a workflow's checkpoints on its durable run, fenced by the claim's epoch.
#[derive(Clone)]
pub(crate) struct CheckpointSink {
    pub(crate) runs: Arc<dyn DurableRunStore>,
    pub(crate) run_id: String,
    pub(crate) epoch: u64,
}

/// One attempt of a durable workflow run.
///
/// # Example
///
/// ```rust
/// use adk_server::background::WorkflowRunContext;
/// use tokio_util::sync::CancellationToken;
///
/// let run = WorkflowRunContext::detached("run-1", CancellationToken::new());
/// assert_eq!(run.thread_id, "run-1");
/// assert!(!run.resumed);
/// ```
#[derive(Clone)]
#[non_exhaustive]
pub struct WorkflowRunContext {
    /// The run identifier.
    pub run_id: String,
    /// The attempt number, counting recoveries.
    pub attempt: u32,
    /// The checkpoint thread; a graph workflow uses it as its `thread_id`.
    pub thread_id: String,
    /// The last checkpoint the run reported, if any.
    pub checkpoint_id: Option<String>,
    /// Whether this attempt resumes one that stopped.
    pub resumed: bool,
    /// Fires when the run is cancelled.
    pub cancel: CancellationToken,
    pub(crate) sink: Option<CheckpointSink>,
}

impl std::fmt::Debug for WorkflowRunContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowRunContext")
            .field("run_id", &self.run_id)
            .field("attempt", &self.attempt)
            .field("thread_id", &self.thread_id)
            .field("checkpoint_id", &self.checkpoint_id)
            .field("resumed", &self.resumed)
            .finish_non_exhaustive()
    }
}

impl WorkflowRunContext {
    /// A context for a run with no durable store behind it, for tests and direct calls.
    pub fn detached(run_id: impl Into<String>, cancel: CancellationToken) -> Self {
        let run_id = run_id.into();
        Self {
            thread_id: run_id.clone(),
            run_id,
            attempt: 1,
            checkpoint_id: None,
            resumed: false,
            cancel,
            sink: None,
        }
    }

    /// Records `checkpoint_id` on the run, so a recovering worker resumes from it.
    ///
    /// # Errors
    ///
    /// Returns `run_store.stale_epoch` when another worker recovered the run; the
    /// workflow must then stop without writing further results.
    pub async fn record_checkpoint(
        &self,
        checkpoint_id: impl Into<String>,
    ) -> std::result::Result<(), AdkError> {
        let Some(sink) = &self.sink else { return Ok(()) };
        sink.runs
            .set_checkpoint(
                &sink.run_id,
                sink.epoch,
                RunCheckpointRef::Graph {
                    thread_id: self.thread_id.clone(),
                    checkpoint_id: Some(checkpoint_id.into()),
                },
            )
            .await
    }
}

/// A `WorkflowExecutor` holding closures registered by name.
///
/// # Example
///
/// ```rust
/// use adk_server::background::WorkflowRegistry;
/// use serde_json::json;
///
/// let registry = WorkflowRegistry::new()
///     .register("greet", |input, _cancel| async move {
///         Ok(json!({ "greeted": input.get("name").cloned() }))
///     })
///     .register_durable("settle", |_input, run| async move {
///         // A graph workflow would invoke its executor with `run.thread_id` here.
///         Ok(json!({ "thread": run.thread_id, "resumed": run.resumed }))
///     });
/// ```
#[derive(Default)]
pub struct WorkflowRegistry {
    workflows: HashMap<String, Arc<BoxedWorkflow>>,
}

/// The boxed form of a registered workflow.
type BoxedWorkflow = dyn Fn(
        WorkflowState,
        WorkflowRunContext,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::result::Result<Value, String>> + Send>,
    > + Send
    + Sync;

impl WorkflowRegistry {
    /// An empty registry, which accepts no workflow.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `workflow_id`; a recovered run restarts it from its input.
    #[must_use]
    pub fn register<F, Fut>(self, workflow_id: impl Into<String>, workflow: F) -> Self
    where
        F: Fn(WorkflowState, CancellationToken) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = std::result::Result<Value, String>> + Send + 'static,
    {
        self.register_durable(workflow_id, move |input, run| workflow(input, run.cancel))
    }

    /// Registers `workflow_id` with the attempt's [`WorkflowRunContext`], so a recovered
    /// run can resume from its checkpoint thread.
    #[must_use]
    pub fn register_durable<F, Fut>(mut self, workflow_id: impl Into<String>, workflow: F) -> Self
    where
        F: Fn(WorkflowState, WorkflowRunContext) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = std::result::Result<Value, String>> + Send + 'static,
    {
        self.workflows
            .insert(workflow_id.into(), Arc::new(move |input, run| Box::pin(workflow(input, run))));
        self
    }
}

#[async_trait::async_trait]
impl WorkflowExecutor for WorkflowRegistry {
    fn has_workflow(&self, workflow_id: &str) -> bool {
        self.workflows.contains_key(workflow_id)
    }

    async fn execute(
        &self,
        workflow_id: &str,
        input: WorkflowState,
        cancel_token: CancellationToken,
    ) -> std::result::Result<Value, String> {
        let run = WorkflowRunContext::detached(uuid::Uuid::new_v4().to_string(), cancel_token);
        self.execute_durable(workflow_id, input, run).await
    }

    async fn execute_durable(
        &self,
        workflow_id: &str,
        input: WorkflowState,
        run: WorkflowRunContext,
    ) -> std::result::Result<Value, String> {
        match self.workflows.get(workflow_id) {
            Some(workflow) => workflow(input, run).await,
            None => Err(format!("workflow '{workflow_id}' is not registered")),
        }
    }
}

// ---------------------------------------------------------------------------
// Shared Application State for Axum Handlers
// ---------------------------------------------------------------------------

/// Shared state for the background run endpoints, and the embedded worker.
///
/// Holds the run store (in memory by default), the targets runs execute, and the
/// governance control. Unless disabled with
/// [`with_embedded_worker(false)`](Self::with_embedded_worker), a [`DurableWorker`] over
/// the same store and targets starts in this process the first time the server is built
/// inside a Tokio runtime or a run is submitted.
#[derive(Clone)]
pub struct BackgroundState {
    runs: Arc<dyn DurableRunStore>,
    executor: Option<Arc<dyn WorkflowExecutor>>,
    agents: BTreeMap<String, Arc<Runner>>,
    app_name: String,
    /// Shared by every clone, so a control installed by the server builder reaches a
    /// scheduler started earlier from a clone.
    governance: Arc<std::sync::RwLock<Option<GovernanceControl>>>,
    worker: DurableWorkerConfig,
    embedded_worker: bool,
    started: Arc<std::sync::Mutex<Option<WorkerHandle>>>,
}

impl std::fmt::Debug for BackgroundState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackgroundState")
            .field("agents", &self.agents.keys().collect::<Vec<_>>())
            .field("has_executor", &self.executor.is_some())
            .field("app_name", &self.app_name)
            .field("queue", &self.worker.queue)
            .field("embedded_worker", &self.embedded_worker)
            .finish_non_exhaustive()
    }
}

impl Default for BackgroundState {
    fn default() -> Self {
        Self::new()
    }
}

impl BackgroundState {
    /// A state over a fresh [`InMemoryRunStore`] with no targets.
    pub fn new() -> Self {
        Self {
            runs: Arc::new(InMemoryRunStore::new()),
            executor: None,
            agents: BTreeMap::new(),
            app_name: "background".to_string(),
            governance: Arc::default(),
            worker: DurableWorkerConfig::default(),
            embedded_worker: true,
            started: Arc::default(),
        }
    }

    /// Uses `store` for runs; share one store across replicas.
    #[must_use]
    pub fn with_run_store(mut self, store: Arc<dyn DurableRunStore>) -> Self {
        self.runs = store;
        self
    }

    /// Attaches the executor that resolves and runs workflows.
    #[must_use]
    pub fn with_executor(mut self, executor: Arc<dyn WorkflowExecutor>) -> Self {
        self.executor = Some(executor);
        self
    }

    /// Runs whose target is `target` execute through `runner`, in the runner's app.
    #[must_use]
    pub fn with_agent(mut self, target: impl Into<String>, runner: Arc<Runner>) -> Self {
        self.agents.insert(target.into(), runner);
        self
    }

    /// Sets the app name workflow runs are recorded under. Defaults to `background`.
    #[must_use]
    pub fn with_app_name(mut self, app_name: impl Into<String>) -> Self {
        self.app_name = app_name.into();
        self
    }

    /// Refuses submissions, and stops the embedded worker claiming, while `control`
    /// refuses work. [`ServerBuilder`](crate::ServerBuilder) installs
    /// [`ServerConfig::governance`](crate::ServerConfig::governance) when none is set.
    #[must_use]
    pub fn with_governance(self, control: GovernanceControl) -> Self {
        *self.governance.write().unwrap_or_else(|e| e.into_inner()) = Some(control);
        self
    }

    /// Sets the embedded worker's configuration, including the queue runs are enqueued on.
    #[must_use]
    pub fn with_worker_config(mut self, config: DurableWorkerConfig) -> Self {
        self.worker = config;
        self
    }

    /// Whether this process runs a worker; disable it on API-only replicas.
    #[must_use]
    pub fn with_embedded_worker(mut self, enabled: bool) -> Self {
        self.embedded_worker = enabled;
        self
    }

    /// The run store.
    pub fn run_store(&self) -> &Arc<dyn DurableRunStore> {
        &self.runs
    }

    /// The governance control, if one is installed.
    pub fn governance(&self) -> Option<GovernanceControl> {
        self.governance.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Installs `control` unless one is already installed.
    pub(crate) fn governance_or(&self, control: &GovernanceControl) {
        let mut slot = self.governance.write().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Some(control.clone());
        }
    }

    /// A [`DurableWorker`] over this state's store, targets, governance, and configuration.
    pub fn worker(&self) -> DurableWorker {
        let mut worker =
            DurableWorker::new(Arc::clone(&self.runs)).with_config(self.worker.clone());
        for (target, runner) in &self.agents {
            worker = worker.with_agent(target.clone(), Arc::clone(runner));
        }
        if let Some(executor) = &self.executor {
            worker = worker.with_workflows(Arc::clone(executor));
        }
        if let Some(control) = self.governance() {
            worker = worker.with_governance(control);
        }
        worker
    }

    /// Starts the embedded worker once, when enabled and a Tokio runtime is running.
    pub fn ensure_worker_started(&self) {
        if !self.embedded_worker || tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let mut started = self.started.lock().unwrap_or_else(|e| e.into_inner());
        if started.as_ref().is_none_or(WorkerHandle::is_finished) {
            *started = Some(self.worker().spawn());
        }
    }

    fn wake_worker(&self) {
        if let Some(handle) = self.started.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            handle.wake();
        }
    }

    /// Makes a parked run claimable now, for the approval service to call once it
    /// records a decision for the run's held tool call.
    ///
    /// A waiting run is claimed again at once and resumes its session, so the agent
    /// dispatches the held call through the governed path with the decision. A wake
    /// that arrives while the run is still running is kept and applied when it parks.
    ///
    /// # Errors
    ///
    /// Returns `run_store.not_found` for an unknown run, or the store's error.
    pub async fn wake_run(&self, run_id: &str) -> std::result::Result<(), AdkError> {
        self.runs.wake(run_id).await?;
        tracing::info!(run.id = %run_id, "run woken");
        self.wake_worker();
        Ok(())
    }

    /// Wakes every parked run of `app_name` continuing `session_id` for `user_id`, and
    /// returns how many were woken; for an approval service that knows the decision's
    /// session but not its run.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    pub async fn wake_session_runs(
        &self,
        app_name: &str,
        user_id: &str,
        session_id: &str,
    ) -> std::result::Result<usize, AdkError> {
        let waiting = self
            .runs
            .list(
                RunFilter::default()
                    .app_name(app_name)
                    .user_id(user_id)
                    .session_id(session_id)
                    .states([RunState::Waiting]),
            )
            .await?;
        for run in &waiting {
            self.runs.wake(&run.run_id).await?;
            tracing::info!(run.id = %run.run_id, "run woken");
        }
        if !waiting.is_empty() {
            self.wake_worker();
        }
        Ok(waiting.len())
    }

    /// Stops the embedded worker, letting in-flight attempts finish.
    pub async fn shutdown_worker(&self) {
        let handle = self.started.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(handle) = handle {
            handle.shutdown().await;
        }
    }

    /// The app a run of `target` is recorded under: the agent's runner app, or this
    /// state's app name for a workflow.
    fn app_name_for(&self, target: &str) -> String {
        self.agents
            .get(target)
            .map_or_else(|| self.app_name.clone(), |runner| runner.app_name().to_string())
    }

    fn knows_target(&self, target: &str) -> bool {
        self.agents.contains_key(target)
            || self.executor.as_ref().is_some_and(|executor| executor.has_workflow(target))
    }
}

// ---------------------------------------------------------------------------
// REST Endpoint Handlers
// ---------------------------------------------------------------------------

fn problem(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": message.into() }))).into_response()
}

fn store_error(error: &AdkError) -> Response {
    let status =
        StatusCode::from_u16(error.http_status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    problem(status, error.to_string())
}

/// POST /runs — Enqueue a run.
async fn submit_run(
    State(state): State<BackgroundState>,
    caller: Option<Extension<Option<RequestContext>>>,
    Json(request): Json<SubmitRunRequest>,
) -> Response {
    if let Some(control) = state.governance()
        && let Err(error) = control.check()
    {
        return problem(StatusCode::SERVICE_UNAVAILABLE, error.to_string());
    }
    let Some(target) = request.target.or(request.workflow_id) else {
        return problem(StatusCode::BAD_REQUEST, "a run needs a `target` (or `workflowId`)");
    };
    // Rejected before it is queued, rather than accepted and then failed.
    if !state.knows_target(&target) {
        return problem(StatusCode::NOT_FOUND, format!("unknown agent or workflow '{target}'"));
    }
    let app_name = state.app_name_for(&target);
    let authenticated = caller.and_then(|Extension(caller)| caller).map(|caller| caller.user_id);
    let user_id = authenticated.or(request.user_id).unwrap_or_else(|| "anonymous".to_string());

    let mut run = NewRun::new(state.worker.queue.clone(), app_name, user_id, target, request.input)
        .with_max_attempts(request.max_retries.unwrap_or(0).saturating_add(1));
    if let Some(session_id) = request.session_id {
        run = run.with_session_id(session_id);
    }
    if let Some(key) = request.idempotency_key {
        run = run.with_idempotency_key(key);
    }
    if let Some(key) = request.concurrency_key {
        run = run.with_concurrency_key(key, request.on_conflict.unwrap_or_default());
    }
    if let Some(at) = request.not_before {
        run = run.with_not_before(at);
    }

    let enqueued = match state.runs.enqueue(run).await {
        Ok(enqueued) => enqueued,
        Err(error) => return store_error(&error),
    };
    state.ensure_worker_started();
    state.wake_worker();
    let (status, record, existing) = match enqueued {
        Enqueued::Created(record) => (StatusCode::CREATED, record, false),
        Enqueued::Existing(record) => (StatusCode::OK, record, true),
        Enqueued::Rejected { blocking_run_id } => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "an unfinished run holds this concurrency key",
                    "blockingRunId": blocking_run_id,
                })),
            )
                .into_response();
        }
    };
    let response = SubmitRunResponse {
        run_id: record.run_id,
        status: record.state.into(),
        created_at: record.created_at.to_rfc3339(),
        existing,
    };
    (status, Json(response)).into_response()
}

/// `GET /runs` query parameters.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListRunsQuery {
    /// Only runs in this state.
    #[serde(default)]
    pub state: Option<RunState>,
    /// Only runs continuing this session.
    #[serde(default)]
    pub session_id: Option<String>,
    /// At most this many runs; defaults to 100.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// GET /runs — List runs.
async fn list_runs(
    State(state): State<BackgroundState>,
    Query(query): Query<ListRunsQuery>,
) -> Response {
    let mut filter =
        RunFilter::default().queue(state.worker.queue.clone()).limit(query.limit.unwrap_or(100));
    if let Some(run_state) = query.state {
        filter = filter.states([run_state]);
    }
    if let Some(session_id) = query.session_id {
        filter = filter.session_id(session_id);
    }
    match state.runs.list(filter).await {
        Ok(runs) => {
            let runs: Vec<RunStatusResponse> = runs.into_iter().map(Into::into).collect();
            (StatusCode::OK, Json(runs)).into_response()
        }
        Err(error) => store_error(&error),
    }
}

/// GET /runs/{run_id} — Get a run.
async fn get_run_status(
    State(state): State<BackgroundState>,
    Path(run_id): Path<String>,
) -> Response {
    match state.runs.get(&run_id).await {
        Ok(Some(run)) => (StatusCode::OK, Json(RunStatusResponse::from(run))).into_response(),
        Ok(None) => problem(StatusCode::NOT_FOUND, "run not found"),
        Err(error) => store_error(&error),
    }
}

/// DELETE /runs/{run_id} — Cancel a run.
///
/// A queued or waiting run is cancelled at once; a running run is flagged and its
/// worker, on whichever replica, stops it at its next heartbeat.
async fn cancel_run(State(state): State<BackgroundState>, Path(run_id): Path<String>) -> Response {
    if let Err(error) = state.runs.request_cancel(&run_id).await {
        return if error.is_not_found() {
            problem(StatusCode::NOT_FOUND, "run not found")
        } else {
            store_error(&error)
        };
    }
    tracing::info!(run.id = %run_id, "run cancellation requested");
    // A run owned by this process stops now; one owned elsewhere at its worker's next heartbeat.
    if let Some(handle) = state.started.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        handle.heartbeat_now();
    }
    match state.runs.get(&run_id).await {
        Ok(Some(run)) => (StatusCode::OK, Json(RunStatusResponse::from(run))).into_response(),
        Ok(None) => problem(StatusCode::NOT_FOUND, "run not found"),
        Err(error) => store_error(&error),
    }
}

/// POST /runs/{run_id}/wake — Make a parked run claimable now.
async fn wake_run(State(state): State<BackgroundState>, Path(run_id): Path<String>) -> Response {
    if let Err(error) = state.wake_run(&run_id).await {
        return if error.is_not_found() {
            problem(StatusCode::NOT_FOUND, "run not found")
        } else {
            store_error(&error)
        };
    }
    match state.runs.get(&run_id).await {
        Ok(Some(run)) => (StatusCode::OK, Json(RunStatusResponse::from(run))).into_response(),
        Ok(None) => problem(StatusCode::NOT_FOUND, "run not found"),
        Err(error) => store_error(&error),
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Create the background runs router over a fresh in-memory state.
///
/// > **Important:** the router carries no authentication. Anyone who reaches it can
/// > submit and cancel runs. Mount it through
/// > [`ServerBuilder::with_background_runs`](crate::ServerBuilder::with_background_runs),
/// > which applies the server's auth middleware, or behind your own authentication layer.
pub fn background_runs_router() -> Router {
    background_runs_router_with_state(BackgroundState::new())
}

/// Create the background runs router with a pre-configured state.
///
/// Mounts `POST /runs`, `GET /runs`, `GET /runs/{run_id}`, `DELETE /runs/{run_id}`, and
/// `POST /runs/{run_id}/wake`,
/// and starts the embedded worker when a Tokio runtime is running.
///
/// > **Important:** the router carries no authentication; see
/// > [`background_runs_router`].
pub fn background_runs_router_with_state(state: BackgroundState) -> Router {
    state.ensure_worker_started();
    Router::new()
        .route("/runs", post(submit_run).get(list_runs))
        .route("/runs/{run_id}", get(get_run_status).delete(cancel_run))
        .route("/runs/{run_id}/wake", post(wake_run))
        .with_state(state)
}
