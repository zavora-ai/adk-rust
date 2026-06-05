//! # Background Runs and Cron Scheduling
//!
//! This module provides REST endpoints for submitting workflows as background runs
//! and managing cron-scheduled job execution.
//!
//! This module is gated behind the `background` feature flag.
//!
//! ## Background Runs
//!
//! - `POST /runs` — Submit a new background run
//! - `GET /runs/{run_id}` — Get run status
//! - `DELETE /runs/{run_id}` — Cancel a run
//!
//! ## Cron Jobs
//!
//! - `POST /cron` — Create a cron job
//! - `GET /cron` — List all cron jobs
//! - `GET /cron/{job_id}` — Get cron job details
//! - `PATCH /cron/{job_id}` — Pause/resume a cron job
//! - `DELETE /cron/{job_id}` — Delete a cron job
//!
//! ## Usage
//!
//! The routers can be used standalone or merged into an existing Axum application:
//!
//! ```rust,ignore
//! use adk_server::background::{background_runs_router, cron_jobs_router};
//!
//! // Standalone usage
//! let runs = background_runs_router();
//! let cron = cron_jobs_router();
//!
//! // Merge into an existing app
//! let app = axum::Router::new()
//!     .merge(runs)
//!     .merge(cron);
//!
//! // Or with shared state for coordinating runs and cron
//! use adk_server::background::{BackgroundState, CronState, background_runs_router_with_state, cron_jobs_router_with_state};
//!
//! let bg_state = BackgroundState::new();
//! let cron_state = CronState::new(bg_state.clone());
//! let app = axum::Router::new()
//!     .merge(background_runs_router_with_state(bg_state))
//!     .merge(cron_jobs_router_with_state(cron_state));
//! ```

pub mod cron;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

pub use cron::{
    ConcurrencyPolicy, CreateCronJobRequest, CronJob, CronJobResponse, CronJobStatus, CronState,
    cron_jobs_router, cron_jobs_router_with_state, start_cron_scheduler, validate_cron_expression,
};

// ---------------------------------------------------------------------------
// Data Types
// ---------------------------------------------------------------------------

/// Workflow input state — a map of string keys to JSON values.
pub type WorkflowState = HashMap<String, Value>;

/// Run lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// Persisted record for a background run.
#[derive(Debug, Clone)]
pub struct BackgroundRun {
    pub run_id: String,
    pub workflow_id: String,
    pub status: RunStatus,
    pub input: WorkflowState,
    pub result: Option<Value>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub timeout: Option<Duration>,
    pub max_retries: u32,
    pub retry_count: u32,
    pub cancel_token: CancellationToken,
}

/// POST /runs request body.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitRunRequest {
    pub workflow_id: String,
    pub input: WorkflowState,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub max_retries: Option<u32>,
}

/// POST /runs response.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitRunResponse {
    pub run_id: String,
    pub status: RunStatus,
    pub created_at: String,
}

/// GET /runs/{run_id} response.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStatusResponse {
    pub run_id: String,
    pub status: RunStatus,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retries_remaining: Option<u32>,
}

// ---------------------------------------------------------------------------
// In-Memory Run Store
// ---------------------------------------------------------------------------

/// Thread-safe in-memory store for background runs.
#[derive(Debug, Clone, Default)]
pub struct RunStore {
    runs: Arc<RwLock<HashMap<String, BackgroundRun>>>,
}

impl RunStore {
    /// Create a new empty run store.
    pub fn new() -> Self {
        Self { runs: Arc::new(RwLock::new(HashMap::new())) }
    }

    /// Insert a new run into the store.
    pub async fn insert(&self, run: BackgroundRun) {
        self.runs.write().await.insert(run.run_id.clone(), run);
    }

    /// Get a run by ID.
    pub async fn get(&self, run_id: &str) -> Option<BackgroundRun> {
        self.runs.read().await.get(run_id).cloned()
    }

    /// Update the status of a run.
    pub async fn update_status(&self, run_id: &str, status: RunStatus) {
        if let Some(run) = self.runs.write().await.get_mut(run_id) {
            run.status = status;
            run.updated_at = Utc::now();
        }
    }

    /// Update a run with a result on completion.
    pub async fn set_completed(&self, run_id: &str, result: Value) {
        if let Some(run) = self.runs.write().await.get_mut(run_id) {
            run.status = RunStatus::Completed;
            run.result = Some(result);
            run.updated_at = Utc::now();
        }
    }

    /// Update a run with an error on failure.
    pub async fn set_failed(&self, run_id: &str, error: String) {
        if let Some(run) = self.runs.write().await.get_mut(run_id) {
            run.status = RunStatus::Failed;
            run.error = Some(error);
            run.updated_at = Utc::now();
        }
    }

    /// Increment the retry count and re-queue the run.
    pub async fn retry(&self, run_id: &str) -> bool {
        if let Some(run) = self.runs.write().await.get_mut(run_id) {
            if run.retry_count < run.max_retries {
                run.retry_count += 1;
                run.status = RunStatus::Queued;
                run.error = None;
                run.updated_at = Utc::now();
                return true;
            }
        }
        false
    }
}

// ---------------------------------------------------------------------------
// Background Runner
// ---------------------------------------------------------------------------

/// Orchestrates background run execution with timeout, retry, and cancellation.
///
/// The `BackgroundRunner` spawns tokio tasks for each submitted run, enforces
/// timeout policies, and handles retry logic by re-enqueuing failed runs from
/// their last checkpoint.
#[derive(Debug, Clone)]
pub struct BackgroundRunner {
    store: RunStore,
}

impl BackgroundRunner {
    /// Create a new background runner backed by the given store.
    pub fn new(store: RunStore) -> Self {
        Self { store }
    }

    /// Submit and execute a background run.
    ///
    /// This transitions the run from `queued` to `running`, executes the workflow
    /// with timeout enforcement, and transitions to `completed`, `failed`, or
    /// `cancelled` based on the outcome.
    pub fn execute(&self, run_id: String) {
        let store = self.store.clone();
        tokio::spawn(async move {
            // Retrieve the run record
            let run = match store.get(&run_id).await {
                Some(r) => r,
                None => return,
            };

            let cancel_token = run.cancel_token.clone();
            let timeout_duration = run.timeout;

            // Transition to running
            store.update_status(&run_id, RunStatus::Running).await;

            // Execute with timeout and cancellation
            let result = Self::run_with_timeout(timeout_duration, &cancel_token).await;

            match result {
                RunOutcome::Completed(value) => {
                    store.set_completed(&run_id, value).await;
                }
                RunOutcome::Failed(error) => {
                    // Attempt retry
                    if store.retry(&run_id).await {
                        // Re-execute after retry
                        let store_clone = store.clone();
                        let run_id_clone = run_id.clone();
                        tokio::spawn(async move {
                            let runner = BackgroundRunner::new(store_clone);
                            runner.execute(run_id_clone);
                        });
                    } else {
                        store.set_failed(&run_id, error).await;
                    }
                }
                RunOutcome::Cancelled => {
                    store.update_status(&run_id, RunStatus::Cancelled).await;
                }
                RunOutcome::TimedOut => {
                    store.set_failed(&run_id, "run timed out".to_string()).await;
                }
            }
        });
    }

    /// Execute the workflow with timeout enforcement and cancellation support.
    async fn run_with_timeout(
        timeout_duration: Option<Duration>,
        cancel_token: &CancellationToken,
    ) -> RunOutcome {
        // The actual workflow execution is a placeholder — in a real implementation
        // this would invoke the workflow via the functional API's TaskContext.
        // For now, we simulate immediate completion.
        let work = async {
            // Check cancellation before work
            if cancel_token.is_cancelled() {
                return RunOutcome::Cancelled;
            }
            // Placeholder: immediate success with empty object result
            RunOutcome::Completed(Value::Object(serde_json::Map::new()))
        };

        match timeout_duration {
            Some(duration) => {
                tokio::select! {
                    _ = cancel_token.cancelled() => RunOutcome::Cancelled,
                    result = tokio::time::timeout(duration, work) => {
                        match result {
                            Ok(outcome) => outcome,
                            Err(_) => RunOutcome::TimedOut,
                        }
                    }
                }
            }
            None => {
                tokio::select! {
                    _ = cancel_token.cancelled() => RunOutcome::Cancelled,
                    outcome = work => outcome,
                }
            }
        }
    }
}

/// Outcome of a background run execution.
#[derive(Debug)]
#[allow(dead_code)]
enum RunOutcome {
    Completed(Value),
    Failed(String),
    Cancelled,
    TimedOut,
}

// ---------------------------------------------------------------------------
// Shared Application State for Axum Handlers
// ---------------------------------------------------------------------------

/// Shared state for background run endpoints.
#[derive(Debug, Clone)]
pub struct BackgroundState {
    pub store: RunStore,
    pub runner: BackgroundRunner,
}

impl BackgroundState {
    /// Create a new background state with a fresh store and runner.
    pub fn new() -> Self {
        let store = RunStore::new();
        let runner = BackgroundRunner::new(store.clone());
        Self { store, runner }
    }
}

impl Default for BackgroundState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// REST Endpoint Handlers
// ---------------------------------------------------------------------------

/// POST /runs — Submit a new background run.
async fn submit_run(
    State(state): State<BackgroundState>,
    Json(request): Json<SubmitRunRequest>,
) -> impl IntoResponse {
    let run_id = uuid::Uuid::new_v4().to_string();
    let now = Utc::now();

    let run = BackgroundRun {
        run_id: run_id.clone(),
        workflow_id: request.workflow_id,
        status: RunStatus::Queued,
        input: request.input,
        result: None,
        error: None,
        created_at: now,
        updated_at: now,
        timeout: request.timeout_secs.map(Duration::from_secs),
        max_retries: request.max_retries.unwrap_or(0),
        retry_count: 0,
        cancel_token: CancellationToken::new(),
    };

    state.store.insert(run).await;

    // Start execution
    state.runner.execute(run_id.clone());

    let response =
        SubmitRunResponse { run_id, status: RunStatus::Queued, created_at: now.to_rfc3339() };

    (StatusCode::CREATED, Json(response))
}

/// GET /runs/{run_id} — Get run status.
async fn get_run_status(
    State(state): State<BackgroundState>,
    Path(run_id): Path<String>,
) -> impl IntoResponse {
    match state.store.get(&run_id).await {
        Some(run) => {
            let retries_remaining = if run.max_retries > 0 {
                Some(run.max_retries.saturating_sub(run.retry_count))
            } else {
                None
            };

            let retry_count = if run.max_retries > 0 { Some(run.retry_count) } else { None };

            let response = RunStatusResponse {
                run_id: run.run_id,
                status: run.status,
                created_at: run.created_at.to_rfc3339(),
                updated_at: run.updated_at.to_rfc3339(),
                result: run.result,
                error: run.error,
                retry_count,
                retries_remaining,
            };

            (StatusCode::OK, Json(response)).into_response()
        }
        None => (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "run not found" })))
            .into_response(),
    }
}

/// DELETE /runs/{run_id} — Cancel a run.
async fn cancel_run(
    State(state): State<BackgroundState>,
    Path(run_id): Path<String>,
) -> impl IntoResponse {
    match state.store.get(&run_id).await {
        Some(run) => {
            // If the run is in a terminal state, return current status without modification
            match run.status {
                RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled => {
                    let response = RunStatusResponse {
                        run_id: run.run_id,
                        status: run.status,
                        created_at: run.created_at.to_rfc3339(),
                        updated_at: run.updated_at.to_rfc3339(),
                        result: run.result,
                        error: run.error,
                        retry_count: if run.max_retries > 0 { Some(run.retry_count) } else { None },
                        retries_remaining: if run.max_retries > 0 {
                            Some(run.max_retries.saturating_sub(run.retry_count))
                        } else {
                            None
                        },
                    };
                    (StatusCode::OK, Json(response)).into_response()
                }
                // For queued or running runs, signal cancellation
                RunStatus::Queued | RunStatus::Running => {
                    run.cancel_token.cancel();
                    state.store.update_status(&run_id, RunStatus::Cancelled).await;

                    let updated = state.store.get(&run_id).await.unwrap();
                    let response = RunStatusResponse {
                        run_id: updated.run_id,
                        status: updated.status,
                        created_at: updated.created_at.to_rfc3339(),
                        updated_at: updated.updated_at.to_rfc3339(),
                        result: updated.result,
                        error: updated.error,
                        retry_count: if updated.max_retries > 0 {
                            Some(updated.retry_count)
                        } else {
                            None
                        },
                        retries_remaining: if updated.max_retries > 0 {
                            Some(updated.max_retries.saturating_sub(updated.retry_count))
                        } else {
                            None
                        },
                    };
                    (StatusCode::OK, Json(response)).into_response()
                }
            }
        }
        None => (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "run not found" })))
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Create the background runs router.
///
/// Mounts the following routes:
/// - `POST /runs` — Submit a new background run
/// - `GET /runs/{run_id}` — Get run status
/// - `DELETE /runs/{run_id}` — Cancel a run
pub fn background_runs_router() -> Router {
    let state = BackgroundState::new();
    background_runs_router_with_state(state)
}

/// Create the background runs router with a pre-configured state.
///
/// This is useful for testing or sharing state with other components.
pub fn background_runs_router_with_state(state: BackgroundState) -> Router {
    Router::new()
        .route("/runs", post(submit_run))
        .route("/runs/{run_id}", get(get_run_status).delete(cancel_run))
        .with_state(state)
}
