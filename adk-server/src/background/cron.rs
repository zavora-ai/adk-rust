//! Cron jobs: REST endpoints over a [`ScheduleStore`] and the scheduling loop.
//!
//! A cron job is a [`Schedule`] with a [`ScheduleSpec::Cron`] spec whose run template
//! targets a workflow or agent. The job's name is its schedule id. The scheduler started
//! by [`start_cron_scheduler`] is a [`DurableScheduler`]; run it on every replica.

use std::sync::Arc;

use adk_core::schedule::{MissedFirePolicy, Schedule, ScheduleSpec};
use adk_core::{InMemoryScheduleStore, NewRun, OnConflict, ScheduleStore};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{BackgroundState, DurableScheduler, WorkflowState};

/// Checks that `expression` parses as a cron expression (five, six, or seven fields).
///
/// # Example
///
/// ```rust
/// use adk_server::background::validate_cron_expression;
///
/// assert!(validate_cron_expression("*/5 * * * *").is_ok());
/// assert!(validate_cron_expression("not cron").is_err());
/// ```
///
/// # Errors
///
/// Returns why the expression does not parse.
pub fn validate_cron_expression(expression: &str) -> Result<(), String> {
    cron_spec(expression, "UTC").next_after(Utc::now()).map(|_| ()).map_err(|e| e.to_string())
}

fn cron_spec(expression: &str, timezone: &str) -> ScheduleSpec {
    ScheduleSpec::Cron { expr: expression.to_string(), timezone: timezone.to_string() }
}

/// What a cron job does when a previous run is still active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConcurrencyPolicy {
    /// Skip the occurrence (`OnConflict::Reject` on the job's concurrency key).
    Skip,
    /// Run alongside the active run (no concurrency key).
    Allow,
    /// Queue behind the active run (`OnConflict::Enqueue`).
    Queue,
}

fn default_concurrency_policy() -> ConcurrencyPolicy {
    ConcurrencyPolicy::Skip
}

fn default_timezone() -> String {
    "UTC".to_string()
}

fn default_missed() -> MissedFirePolicy {
    MissedFirePolicy::Skip
}

/// Cron job lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CronJobStatus {
    /// The job fires.
    Active,
    /// The job does not fire; occurrences inside the pause are skipped.
    Paused,
}

/// `POST /cron` request body.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateCronJobRequest {
    /// The job's name, which is also its id.
    pub name: String,
    /// The workflow or agent each firing runs.
    #[serde(alias = "target")]
    pub workflow_id: String,
    /// The cron expression (five, six, or seven fields).
    pub cron_expression: String,
    /// The IANA time zone the expression is evaluated in. Defaults to `UTC`.
    #[serde(default = "default_timezone")]
    pub timezone: String,
    /// The input each run receives.
    #[serde(default)]
    pub input: Option<WorkflowState>,
    /// What happens when a previous run is still active. Defaults to `skip`.
    #[serde(default = "default_concurrency_policy")]
    pub concurrency_policy: ConcurrencyPolicy,
    /// Which occurrences fire after the scheduler fell behind. Defaults to `skip`.
    #[serde(default = "default_missed")]
    pub missed_fire_policy: MissedFirePolicy,
}

/// A cron job as the REST API reports it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CronJobResponse {
    /// The job id, equal to its name.
    pub job_id: String,
    /// The job's name.
    pub name: String,
    /// The workflow or agent each firing runs.
    pub workflow_id: String,
    /// The cron expression; absent for a schedule with another spec.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cron_expression: Option<String>,
    /// The time zone; absent for a schedule with another spec.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// The schedule's spec.
    pub spec: ScheduleSpec,
    /// Whether the job fires.
    pub status: CronJobStatus,
    /// What happens when a previous run is still active.
    pub concurrency_policy: ConcurrencyPolicy,
    /// Which occurrences fire after the scheduler fell behind.
    pub missed_fire_policy: MissedFirePolicy,
    /// The next occurrence to fire.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_fire_at: Option<String>,
    /// The schedule's revision.
    pub revision: u64,
}

impl From<Schedule> for CronJobResponse {
    fn from(schedule: Schedule) -> Self {
        let (cron_expression, timezone) = match &schedule.spec {
            ScheduleSpec::Cron { expr, timezone } => (Some(expr.clone()), Some(timezone.clone())),
            _ => (None, None),
        };
        let concurrency_policy = match (&schedule.run.concurrency_key, schedule.run.on_conflict) {
            (None, _) => ConcurrencyPolicy::Allow,
            (Some(_), OnConflict::Reject) => ConcurrencyPolicy::Skip,
            (Some(_), _) => ConcurrencyPolicy::Queue,
        };
        Self {
            job_id: schedule.id.clone(),
            name: schedule.id,
            workflow_id: schedule.run.target,
            cron_expression,
            timezone,
            spec: schedule.spec,
            status: if schedule.paused { CronJobStatus::Paused } else { CronJobStatus::Active },
            concurrency_policy,
            missed_fire_policy: schedule.missed,
            next_fire_at: schedule.next_fire_at.map(|at| at.to_rfc3339()),
            revision: schedule.revision,
        }
    }
}

/// `PATCH /cron/{job_id}` request body.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchCronJobRequest {
    /// The status to set.
    pub status: CronJobStatus,
}

/// Shared state for the cron endpoints and the scheduler.
#[derive(Clone)]
pub struct CronState {
    /// Where schedules are stored.
    pub schedules: Arc<dyn ScheduleStore>,
    /// The background runs state whose run store the schedules enqueue into.
    pub background_state: BackgroundState,
}

impl std::fmt::Debug for CronState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CronState").field("background_state", &self.background_state).finish()
    }
}

impl CronState {
    /// A cron state over a fresh [`InMemoryScheduleStore`].
    pub fn new(background_state: BackgroundState) -> Self {
        Self { schedules: Arc::new(InMemoryScheduleStore::new()), background_state }
    }

    /// Uses `store` for schedules; share one store across replicas.
    #[must_use]
    pub fn with_schedule_store(mut self, store: Arc<dyn ScheduleStore>) -> Self {
        self.schedules = store;
        self
    }

    /// A [`DurableScheduler`] over this state's stores and current governance control.
    pub fn scheduler(&self) -> DurableScheduler {
        let scheduler = DurableScheduler::new(
            Arc::clone(&self.schedules),
            Arc::clone(self.background_state.run_store()),
        );
        match self.background_state.governance() {
            Some(control) => scheduler.with_governance(control),
            None => scheduler,
        }
    }
}

fn problem(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": message.into() }))).into_response()
}

fn store_error(error: &adk_core::AdkError) -> Response {
    if error.is_not_found() {
        return problem(StatusCode::NOT_FOUND, "cron job not found");
    }
    let status =
        StatusCode::from_u16(error.http_status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    problem(status, error.to_string())
}

/// POST /cron — Create a cron job.
async fn create_cron_job(
    State(state): State<CronState>,
    Json(request): Json<CreateCronJobRequest>,
) -> Response {
    let spec = cron_spec(&request.cron_expression, &request.timezone);
    let first = match spec.next_after(Utc::now()) {
        Ok(first) => first,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "invalid cron expression",
                    "expression": request.cron_expression,
                    "reason": error.to_string(),
                })),
            )
                .into_response();
        }
    };
    match state.schedules.get(&request.name).await {
        Ok(Some(_)) => {
            return problem(
                StatusCode::CONFLICT,
                format!("a cron job named '{}' already exists", request.name),
            );
        }
        Ok(None) => {}
        Err(error) => return store_error(&error),
    }
    let background = &state.background_state;
    let input = request.input.map_or(Value::Object(Default::default()), |input| {
        Value::Object(input.into_iter().collect())
    });
    let mut run = NewRun::new(
        background.worker.queue.clone(),
        background.app_name_for(&request.workflow_id),
        "cron",
        request.workflow_id,
        input,
    );
    let concurrency_key = format!("cron:{}", request.name);
    run = match request.concurrency_policy {
        ConcurrencyPolicy::Skip => run.with_concurrency_key(concurrency_key, OnConflict::Reject),
        ConcurrencyPolicy::Queue => run.with_concurrency_key(concurrency_key, OnConflict::Enqueue),
        ConcurrencyPolicy::Allow => run,
    };
    let mut schedule = Schedule::new(request.name, spec, run, request.missed_fire_policy);
    if let Some(first) = first {
        schedule = schedule.with_next_fire_at(first);
    }
    match state.schedules.upsert(schedule).await {
        Ok(stored) => {
            tracing::info!(schedule.id = %stored.id, "cron job created");
            (StatusCode::CREATED, Json(CronJobResponse::from(stored))).into_response()
        }
        Err(error) => problem(StatusCode::BAD_REQUEST, error.to_string()),
    }
}

/// GET /cron — List cron jobs.
async fn list_cron_jobs(State(state): State<CronState>) -> Response {
    match state.schedules.list().await {
        Ok(schedules) => {
            let jobs: Vec<CronJobResponse> = schedules.into_iter().map(Into::into).collect();
            (StatusCode::OK, Json(jobs)).into_response()
        }
        Err(error) => store_error(&error),
    }
}

/// GET /cron/{job_id} — Get a cron job.
async fn get_cron_job(State(state): State<CronState>, Path(job_id): Path<String>) -> Response {
    match state.schedules.get(&job_id).await {
        Ok(Some(schedule)) => {
            (StatusCode::OK, Json(CronJobResponse::from(schedule))).into_response()
        }
        Ok(None) => problem(StatusCode::NOT_FOUND, format!("cron job '{job_id}' not found")),
        Err(error) => store_error(&error),
    }
}

/// PATCH /cron/{job_id} — Pause or resume a cron job.
///
/// Resuming moves the next fire time to the first occurrence after now, so occurrences
/// that fell inside the pause are skipped rather than replayed.
async fn patch_cron_job(
    State(state): State<CronState>,
    Path(job_id): Path<String>,
    Json(request): Json<PatchCronJobRequest>,
) -> Response {
    let result = match request.status {
        CronJobStatus::Paused => state.schedules.set_paused(&job_id, true).await,
        CronJobStatus::Active => match state.schedules.get(&job_id).await {
            Ok(Some(schedule)) if schedule.paused => {
                let next = match schedule.spec.next_after(Utc::now()) {
                    Ok(next) => next,
                    Err(error) => return problem(StatusCode::BAD_REQUEST, error.to_string()),
                };
                let mut resumed = schedule.with_paused(false);
                resumed.next_fire_at = next;
                state.schedules.upsert(resumed).await
            }
            Ok(Some(schedule)) => Ok(schedule),
            Ok(None) => return problem(StatusCode::NOT_FOUND, "cron job not found"),
            Err(error) => Err(error),
        },
    };
    match result {
        Ok(schedule) => (StatusCode::OK, Json(CronJobResponse::from(schedule))).into_response(),
        Err(error) => store_error(&error),
    }
}

/// DELETE /cron/{job_id} — Delete a cron job.
async fn delete_cron_job(State(state): State<CronState>, Path(job_id): Path<String>) -> Response {
    match state.schedules.delete(&job_id).await {
        Ok(()) => (StatusCode::NO_CONTENT, ()).into_response(),
        Err(error) => store_error(&error),
    }
}

/// Starts the scheduling loop: a [`DurableScheduler`] tick every second over the state's
/// schedule and run stores. Run it on every replica; each fire time is enqueued once.
///
/// The governance control is read on every tick, so one installed later by
/// [`ServerBuilder`](crate::ServerBuilder) applies.
pub fn start_cron_scheduler(state: CronState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            if let Err(error) = state.scheduler().tick().await {
                tracing::warn!(error = %error, "scheduler tick failed");
            }
        }
    })
}

/// Create the cron jobs router over a fresh in-memory schedule store.
///
/// > **Important:** the router carries no authentication. Anyone who reaches it can
/// > schedule recurring work. Mount it through
/// > [`ServerBuilder::with_cron_jobs`](crate::ServerBuilder::with_cron_jobs), which
/// > applies the server's auth middleware, or behind your own authentication layer.
pub fn cron_jobs_router(background_state: BackgroundState) -> Router {
    cron_jobs_router_with_state(CronState::new(background_state))
}

/// Create the cron jobs router with a pre-configured state.
///
/// > **Important:** the router carries no authentication; see [`cron_jobs_router`].
pub fn cron_jobs_router_with_state(state: CronState) -> Router {
    Router::new()
        .route("/cron", post(create_cron_job).get(list_cron_jobs))
        .route("/cron/{job_id}", get(get_cron_job).patch(patch_cron_job).delete(delete_cron_job))
        .with_state(state)
}
