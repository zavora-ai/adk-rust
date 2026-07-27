//! Cron job scheduling: expression validation, data types, REST endpoints, and scheduling loop.
//!
//! This module provides:
//! - Cron expression validation and parsing (5-field and 6-field)
//! - In-memory cron job store with metadata tracking
//! - REST endpoints for CRUD operations on cron jobs
//! - Background scheduling loop with concurrency control

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use cron::Schedule;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use super::{BackgroundState, RunStatus, WorkflowState};

// ---------------------------------------------------------------------------
// Cron Expression Validation (Task 11.1)
// ---------------------------------------------------------------------------

/// Validate and parse a cron expression.
///
/// Supports both 5-field (minute, hour, day-of-month, month, day-of-week)
/// and 6-field (seconds, minute, hour, day-of-month, month, day-of-week)
/// cron expressions. The `cron` crate handles both natively.
///
/// # Errors
///
/// Returns an error string if the expression cannot be parsed.
pub fn validate_cron_expression(expression: &str) -> Result<Schedule, String> {
    Schedule::from_str(expression).map_err(|e| format!("invalid cron expression: {e}"))
}

// ---------------------------------------------------------------------------
// Cron Job Data Types (Task 11.3)
// ---------------------------------------------------------------------------

/// Cron job concurrency behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConcurrencyPolicy {
    Skip,
    Allow,
    Queue,
}

/// Default concurrency policy is `Skip`.
fn default_concurrency_policy() -> ConcurrencyPolicy {
    ConcurrencyPolicy::Skip
}

/// Cron job lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CronJobStatus {
    Active,
    Paused,
}

/// POST /cron request body.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateCronJobRequest {
    pub name: String,
    pub workflow_id: String,
    pub cron_expression: String,
    #[serde(default)]
    pub input: Option<WorkflowState>,
    #[serde(default = "default_concurrency_policy")]
    pub concurrency_policy: ConcurrencyPolicy,
}

/// GET /cron response item.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CronJobResponse {
    pub job_id: String,
    pub name: String,
    pub workflow_id: String,
    pub cron_expression: String,
    pub status: CronJobStatus,
    pub concurrency_policy: ConcurrencyPolicy,
    pub created_at: String,
    pub last_execution: Option<String>,
    pub execution_count: u64,
    pub active_run_count: u32,
}

/// PATCH /cron/{job_id} request body.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchCronJobRequest {
    pub status: CronJobStatus,
}

/// Persisted record for a cron job.
#[derive(Debug, Clone)]
pub struct CronJob {
    pub job_id: String,
    pub name: String,
    pub workflow_id: String,
    pub cron_expression: String,
    pub input: Option<WorkflowState>,
    pub status: CronJobStatus,
    pub concurrency_policy: ConcurrencyPolicy,
    pub created_at: DateTime<Utc>,
    pub last_execution: Option<DateTime<Utc>>,
    pub execution_count: u64,
    pub active_run_count: u32,
    /// Queued runs waiting to execute (for `Queue` concurrency policy).
    pub queued_runs: Vec<String>,
}

impl CronJob {
    /// Convert this cron job to a response DTO.
    fn to_response(&self) -> CronJobResponse {
        CronJobResponse {
            job_id: self.job_id.clone(),
            name: self.name.clone(),
            workflow_id: self.workflow_id.clone(),
            cron_expression: self.cron_expression.clone(),
            status: self.status,
            concurrency_policy: self.concurrency_policy,
            created_at: self.created_at.to_rfc3339(),
            last_execution: self.last_execution.map(|t| t.to_rfc3339()),
            execution_count: self.execution_count,
            active_run_count: self.active_run_count,
        }
    }
}

// ---------------------------------------------------------------------------
// In-Memory Cron Job Store
// ---------------------------------------------------------------------------

/// Thread-safe in-memory store for cron jobs.
#[derive(Debug, Clone, Default)]
pub struct CronJobStore {
    jobs: Arc<RwLock<HashMap<String, CronJob>>>,
}

impl CronJobStore {
    /// Create a new empty cron job store.
    pub fn new() -> Self {
        Self { jobs: Arc::new(RwLock::new(HashMap::new())) }
    }

    /// Insert a new cron job into the store.
    pub async fn insert(&self, job: CronJob) {
        self.jobs.write().await.insert(job.job_id.clone(), job);
    }

    /// Get a cron job by ID.
    pub async fn get(&self, job_id: &str) -> Option<CronJob> {
        self.jobs.read().await.get(job_id).cloned()
    }

    /// List all cron jobs.
    pub async fn list(&self) -> Vec<CronJob> {
        self.jobs.read().await.values().cloned().collect()
    }

    /// Update the status of a cron job. Returns `true` if the job existed.
    pub async fn update_status(&self, job_id: &str, status: CronJobStatus) -> bool {
        if let Some(job) = self.jobs.write().await.get_mut(job_id) {
            job.status = status;
            true
        } else {
            false
        }
    }

    /// Remove a cron job by ID. Returns `true` if the job existed.
    pub async fn remove(&self, job_id: &str) -> bool {
        self.jobs.write().await.remove(job_id).is_some()
    }

    /// Record that a job executed, setting `last_execution` to now.
    ///
    /// The scheduler no longer calls this: it advances scheduling state with
    /// [`CronJobStore::claim_occurrence`], which stores the exact schedule point rather
    /// than the wall-clock time a run happened to start. Setting `last_execution` to
    /// now would skip any occurrence between the schedule point and that moment.
    pub async fn record_execution(&self, job_id: &str) {
        if let Some(job) = self.jobs.write().await.get_mut(job_id) {
            job.last_execution = Some(Utc::now());
            job.execution_count += 1;
        }
    }

    /// Increment the active run count for a cron job.
    pub async fn increment_active_runs(&self, job_id: &str) {
        if let Some(job) = self.jobs.write().await.get_mut(job_id) {
            job.active_run_count += 1;
        }
    }

    /// Decrement the active run count for a cron job.
    pub async fn decrement_active_runs(&self, job_id: &str) {
        if let Some(job) = self.jobs.write().await.get_mut(job_id) {
            job.active_run_count = job.active_run_count.saturating_sub(1);
        }
    }

    /// Enqueue a run for a cron job (for `Queue` policy).
    pub async fn enqueue_run(&self, job_id: &str, run_id: String) {
        if let Some(job) = self.jobs.write().await.get_mut(job_id) {
            job.queued_runs.push(run_id);
        }
    }

    /// Dequeue the next pending run for a cron job (for `Queue` policy).
    pub async fn dequeue_run(&self, job_id: &str) -> Option<String> {
        if let Some(job) = self.jobs.write().await.get_mut(job_id)
            && !job.queued_runs.is_empty()
        {
            return Some(job.queued_runs.remove(0));
        }
        None
    }

    /// Get all active jobs that are due for execution.
    pub async fn get_due_jobs(&self) -> Vec<CronJob> {
        self.due_occurrences().await.into_iter().map(|(job, _)| job).collect()
    }

    /// Active jobs that are due, paired with the exact occurrence they are due for.
    ///
    /// The occurrence timestamp is what makes a claim idempotent: without it, a job
    /// whose scheduling state has not advanced looks due on every poll.
    pub async fn due_occurrences(&self) -> Vec<(CronJob, DateTime<Utc>)> {
        let jobs = self.jobs.read().await;
        let now = Utc::now();

        jobs.values()
            .filter(|job| job.status == CronJobStatus::Active)
            .filter_map(|job| {
                let schedule = Schedule::from_str(&job.cron_expression).ok()?;
                let reference_time = job.last_execution.unwrap_or(job.created_at);
                let next = schedule.after(&reference_time).next()?;
                (next <= now).then(|| (job.clone(), next))
            })
            .collect()
    }

    /// Claim `occurrence` for `job_id`, returning whether this caller won it.
    ///
    /// Scheduling state advances here rather than when a run starts, so an occurrence
    /// that is queued behind an active run is not offered again on the next poll. The
    /// check and the write happen under one write lock, so only one caller can claim a
    /// given occurrence.
    pub async fn claim_occurrence(&self, job_id: &str, occurrence: DateTime<Utc>) -> bool {
        let mut jobs = self.jobs.write().await;
        let Some(job) = jobs.get_mut(job_id) else {
            return false;
        };
        if job.last_execution.is_some_and(|last| last >= occurrence) {
            return false;
        }
        job.last_execution = Some(occurrence);
        true
    }
}

// ---------------------------------------------------------------------------
// Shared Cron State for Axum Handlers
// ---------------------------------------------------------------------------

/// Shared state for cron job endpoints.
#[derive(Debug, Clone)]
pub struct CronState {
    pub cron_store: CronJobStore,
    pub background_state: BackgroundState,
}

impl CronState {
    /// Create a new cron state with a fresh store.
    pub fn new(background_state: BackgroundState) -> Self {
        Self { cron_store: CronJobStore::new(), background_state }
    }
}

// ---------------------------------------------------------------------------
// REST Endpoint Handlers (Task 11.4)
// ---------------------------------------------------------------------------

/// POST /cron — Create a new cron job.
async fn create_cron_job(
    State(state): State<CronState>,
    Json(request): Json<CreateCronJobRequest>,
) -> impl IntoResponse {
    // Validate cron expression
    if let Err(reason) = validate_cron_expression(&request.cron_expression) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid cron expression",
                "expression": request.cron_expression,
                "reason": reason,
            })),
        )
            .into_response();
    }

    let job_id = uuid::Uuid::new_v4().to_string();
    let now = Utc::now();

    let job = CronJob {
        job_id: job_id.clone(),
        name: request.name,
        workflow_id: request.workflow_id,
        cron_expression: request.cron_expression,
        input: request.input,
        status: CronJobStatus::Active,
        concurrency_policy: request.concurrency_policy,
        created_at: now,
        last_execution: None,
        execution_count: 0,
        active_run_count: 0,
        queued_runs: Vec::new(),
    };

    let response = job.to_response();
    state.cron_store.insert(job).await;

    (StatusCode::CREATED, Json(serde_json::to_value(response).unwrap())).into_response()
}

/// GET /cron — List all cron jobs.
async fn list_cron_jobs(State(state): State<CronState>) -> impl IntoResponse {
    let jobs = state.cron_store.list().await;
    let responses: Vec<CronJobResponse> = jobs.iter().map(|j| j.to_response()).collect();
    (StatusCode::OK, Json(serde_json::to_value(responses).unwrap())).into_response()
}

/// GET /cron/{job_id} — Retrieve one cron job.
async fn get_cron_job(
    State(state): State<CronState>,
    Path(job_id): Path<String>,
) -> impl IntoResponse {
    match state.cron_store.get(&job_id).await {
        Some(job) => {
            (StatusCode::OK, Json(serde_json::to_value(job.to_response()).unwrap())).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": format!("cron job '{job_id}' not found") })),
        )
            .into_response(),
    }
}

/// PATCH /cron/{job_id} — Pause or resume a cron job.
async fn patch_cron_job(
    State(state): State<CronState>,
    Path(job_id): Path<String>,
    Json(request): Json<PatchCronJobRequest>,
) -> impl IntoResponse {
    if state.cron_store.update_status(&job_id, request.status).await {
        match state.cron_store.get(&job_id).await {
            Some(job) => (StatusCode::OK, Json(serde_json::to_value(job.to_response()).unwrap()))
                .into_response(),
            None => {
                (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "cron job not found" })))
                    .into_response()
            }
        }
    } else {
        (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "cron job not found" })))
            .into_response()
    }
}

/// DELETE /cron/{job_id} — Delete a cron job.
async fn delete_cron_job(
    State(state): State<CronState>,
    Path(job_id): Path<String>,
) -> impl IntoResponse {
    if state.cron_store.remove(&job_id).await {
        (StatusCode::NO_CONTENT, ()).into_response()
    } else {
        (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "cron job not found" })))
            .into_response()
    }
}

// ---------------------------------------------------------------------------
// Cron Job Scheduling Loop (Task 11.5)
// ---------------------------------------------------------------------------

/// Start the background cron scheduling loop.
///
/// This spawns a tokio task that checks every second for cron jobs that are due
/// for execution, then triggers background runs based on concurrency policy.
pub fn start_cron_scheduler(state: CronState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));

        loop {
            interval.tick().await;

            for (job, occurrence) in state.cron_store.due_occurrences().await {
                // Claim the occurrence before acting on it. Under `Queue` the previous
                // code advanced scheduling state only when a run started, so an
                // occurrence waiting behind an active run was enqueued again on every
                // one-second poll and one schedule point became many runs.
                if !state.cron_store.claim_occurrence(&job.job_id, occurrence).await {
                    continue;
                }

                match job.concurrency_policy {
                    ConcurrencyPolicy::Skip => {
                        // Drop this occurrence if a previous run is still active.
                        if job.active_run_count > 0 {
                            continue;
                        }
                        trigger_run(&state, &job).await;
                    }
                    ConcurrencyPolicy::Allow => {
                        trigger_run(&state, &job).await;
                    }
                    ConcurrencyPolicy::Queue => {
                        if job.active_run_count > 0 {
                            let run_id = uuid::Uuid::new_v4().to_string();
                            state.cron_store.enqueue_run(&job.job_id, run_id).await;
                        } else {
                            trigger_run(&state, &job).await;
                        }
                    }
                }
            }
        }
    })
}

/// Builds a background run record for one occurrence of `job`.
fn run_record(job: &CronJob, run_id: String) -> super::BackgroundRun {
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    let now = Utc::now();
    super::BackgroundRun {
        run_id,
        workflow_id: job.workflow_id.clone(),
        status: RunStatus::Queued,
        input: job.input.clone().unwrap_or_default(),
        result: None,
        error: None,
        created_at: now,
        updated_at: now,
        timeout: Some(Duration::from_secs(3600)), // 1 hour default timeout for cron runs
        max_retries: 0,
        retry_count: 0,
        cancel_token: CancellationToken::new(),
    }
}

/// Trigger a background run for a due cron job.
///
/// Every run for a job goes through here, including one started from the queue, so the
/// active count is always paired with a monitor that will decrement it.
async fn trigger_run(state: &CronState, job: &CronJob) {
    let run_id = uuid::Uuid::new_v4().to_string();

    state.background_state.store.insert(run_record(job, run_id.clone())).await;
    state.cron_store.increment_active_runs(&job.job_id).await;
    state.background_state.runner.execute(run_id.clone());

    // One monitor follows the whole chain: the run it started, then each run it takes
    // off the queue. Starting a queued run without a monitor left `active_run_count`
    // permanently nonzero, which stalled every later Skip and Queue decision.
    let state = state.clone();
    let job_id = job.job_id.clone();
    tokio::spawn(async move {
        let mut current = run_id;
        loop {
            if !wait_for_run(&state, &current).await {
                // The run record vanished; release the slot so the job is not wedged.
                state.cron_store.decrement_active_runs(&job_id).await;
                break;
            }
            state.cron_store.decrement_active_runs(&job_id).await;

            let Some(job) = state.cron_store.get(&job_id).await else {
                break;
            };
            if job.concurrency_policy != ConcurrencyPolicy::Queue {
                break;
            }
            let Some(next_run_id) = state.cron_store.dequeue_run(&job_id).await else {
                break;
            };

            state.background_state.store.insert(run_record(&job, next_run_id.clone())).await;
            state.cron_store.increment_active_runs(&job_id).await;
            state.background_state.runner.execute(next_run_id.clone());
            current = next_run_id;
        }
    });
}

/// Waits for `run_id` to reach a terminal status.
///
/// Returns `false` when the run record disappeared, which is not a completion and must
/// not be mistaken for one.
async fn wait_for_run(state: &CronState, run_id: &str) -> bool {
    use std::time::Duration;
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        match state.background_state.store.get(run_id).await {
            Some(run) => match run.status {
                RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled => return true,
                _ => continue,
            },
            None => return false,
        }
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Create the cron jobs router.
///
/// Mounts the following routes:
/// - `POST /cron` — Create a new cron job
/// - `GET /cron` — List all cron jobs
/// - `PATCH /cron/{job_id}` — Pause/resume a cron job
/// - `DELETE /cron/{job_id}` — Delete a cron job
pub fn cron_jobs_router(background_state: BackgroundState) -> Router {
    let state = CronState::new(background_state);
    cron_jobs_router_with_state(state)
}

/// Create the cron jobs router with a pre-configured state.
///
/// This is useful for testing or sharing state with other components.
pub fn cron_jobs_router_with_state(state: CronState) -> Router {
    Router::new()
        .route("/cron", post(create_cron_job).get(list_cron_jobs))
        .route("/cron/{job_id}", get(get_cron_job).patch(patch_cron_job).delete(delete_cron_job))
        .with_state(state)
}
