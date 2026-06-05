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
    routing::post,
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

    /// Record an execution for a cron job (updates last_execution, increments count).
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
        if let Some(job) = self.jobs.write().await.get_mut(job_id) {
            if !job.queued_runs.is_empty() {
                return Some(job.queued_runs.remove(0));
            }
        }
        None
    }

    /// Get all active jobs that are due for execution.
    pub async fn get_due_jobs(&self) -> Vec<CronJob> {
        let jobs = self.jobs.read().await;
        let now = Utc::now();

        jobs.values()
            .filter(|job| job.status == CronJobStatus::Active)
            .filter(|job| {
                // Check if the job is due based on cron expression
                if let Ok(schedule) = Schedule::from_str(&job.cron_expression) {
                    // Find the next occurrence after the last execution (or creation time)
                    let reference_time = job.last_execution.unwrap_or(job.created_at);
                    if let Some(next) = schedule.after(&reference_time).next() {
                        return next <= now;
                    }
                }
                false
            })
            .cloned()
            .collect()
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

            let due_jobs = state.cron_store.get_due_jobs().await;

            for job in due_jobs {
                match job.concurrency_policy {
                    ConcurrencyPolicy::Skip => {
                        // Skip if previous run still active
                        if job.active_run_count > 0 {
                            continue;
                        }
                        trigger_run(&state, &job).await;
                    }
                    ConcurrencyPolicy::Allow => {
                        // Always create a new run
                        trigger_run(&state, &job).await;
                    }
                    ConcurrencyPolicy::Queue => {
                        // If active, enqueue; otherwise execute immediately
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

/// Trigger a background run for a due cron job.
async fn trigger_run(state: &CronState, job: &CronJob) {
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    let run_id = uuid::Uuid::new_v4().to_string();
    let now = Utc::now();

    let run = super::BackgroundRun {
        run_id: run_id.clone(),
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
    };

    state.background_state.store.insert(run).await;
    state.cron_store.record_execution(&job.job_id).await;
    state.cron_store.increment_active_runs(&job.job_id).await;

    // Start execution
    state.background_state.runner.execute(run_id.clone());

    // Spawn a task to monitor run completion and handle queue policy
    let cron_store = state.cron_store.clone();
    let bg_store = state.background_state.store.clone();
    let bg_runner = state.background_state.runner.clone();
    let job_id = job.job_id.clone();

    tokio::spawn(async move {
        // Poll until the run completes
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if let Some(run) = bg_store.get(&run_id).await {
                match run.status {
                    RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled => {
                        cron_store.decrement_active_runs(&job_id).await;

                        // If queue policy, check for queued runs
                        if let Some(job) = cron_store.get(&job_id).await {
                            if job.concurrency_policy == ConcurrencyPolicy::Queue {
                                if let Some(queued_run_id) = cron_store.dequeue_run(&job_id).await {
                                    // Create and execute the queued run
                                    let now = Utc::now();
                                    let queued_run = super::BackgroundRun {
                                        run_id: queued_run_id.clone(),
                                        workflow_id: job.workflow_id.clone(),
                                        status: RunStatus::Queued,
                                        input: job.input.clone().unwrap_or_default(),
                                        result: None,
                                        error: None,
                                        created_at: now,
                                        updated_at: now,
                                        timeout: Some(Duration::from_secs(3600)),
                                        max_retries: 0,
                                        retry_count: 0,
                                        cancel_token: CancellationToken::new(),
                                    };
                                    bg_store.insert(queued_run).await;
                                    cron_store.increment_active_runs(&job_id).await;
                                    bg_runner.execute(queued_run_id);
                                }
                            }
                        }
                        break;
                    }
                    _ => continue,
                }
            } else {
                break;
            }
        }
    });
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
        .route("/cron/{job_id}", axum::routing::patch(patch_cron_job).delete(delete_cron_job))
        .with_state(state)
}
