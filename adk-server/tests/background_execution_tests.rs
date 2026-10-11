//! Background runs execute real work through the embedded durable worker, and the cron
//! routes manage schedules in the schedule store.

#![cfg(feature = "background")]

use std::sync::Arc;
use std::time::Duration;

use adk_core::{RunFilter, RunState};
use adk_server::background::{
    BackgroundState, CronState, WorkflowRegistry, background_runs_router_with_state,
    cron_jobs_router_with_state, start_cron_scheduler,
};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn call(app: Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let builder = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn await_terminal(state: &BackgroundState, run_id: &str) -> Value {
    for _ in 0..500 {
        let (status, body) = call(
            background_runs_router_with_state(state.clone()),
            "GET",
            &format!("/runs/{run_id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        if matches!(body["status"].as_str(), Some("completed" | "failed" | "cancelled")) {
            return body;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("run {run_id} never finished");
}

fn registry() -> WorkflowRegistry {
    WorkflowRegistry::new()
        .register("echo", |input, _cancel| async move {
            Ok(json!({ "echoed": input.get("name").cloned().unwrap_or(Value::Null) }))
        })
        .register("boom", |_input, _cancel| async move { Err("exploded".to_string()) })
}

#[tokio::test]
async fn a_run_executes_its_workflow_and_returns_the_output() {
    let state = BackgroundState::new().with_executor(Arc::new(registry()));
    let (status, submitted) = call(
        background_runs_router_with_state(state.clone()),
        "POST",
        "/runs",
        Some(json!({ "workflowId": "echo", "input": { "name": "ada" } })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let run = await_terminal(&state, submitted["runId"].as_str().unwrap()).await;
    assert_eq!(run["status"], "completed");
    assert_eq!(run["result"], json!({ "echoed": "ada" }));
    assert_eq!(run["attempt"], 1);
    state.shutdown_worker().await;
}

#[tokio::test]
async fn a_failing_workflow_fails_the_run_after_its_retries() {
    let state = BackgroundState::new().with_executor(Arc::new(registry()));
    let (_, submitted) = call(
        background_runs_router_with_state(state.clone()),
        "POST",
        "/runs",
        Some(json!({ "target": "boom", "input": {}, "maxRetries": 1 })),
    )
    .await;
    let run = await_terminal(&state, submitted["runId"].as_str().unwrap()).await;
    assert_eq!(run["status"], "failed");
    assert_eq!(run["error"], "exploded");
    assert_eq!(run["attempt"], 2);
    assert_eq!(run["retriesRemaining"], 0);
    state.shutdown_worker().await;
}

#[tokio::test]
async fn an_unknown_target_is_rejected_instead_of_queued() {
    let state = BackgroundState::new().with_executor(Arc::new(registry()));
    let (status, _) = call(
        background_runs_router_with_state(state.clone()),
        "POST",
        "/runs",
        Some(json!({ "workflowId": "missing", "input": {} })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(state.run_store().list(RunFilter::default()).await.unwrap().is_empty());
}

#[tokio::test]
async fn cancelling_a_queued_run_cancels_it_at_once() {
    let state =
        BackgroundState::new().with_executor(Arc::new(registry())).with_embedded_worker(false);
    let (_, submitted) = call(
        background_runs_router_with_state(state.clone()),
        "POST",
        "/runs",
        Some(json!({ "target": "echo", "input": {} })),
    )
    .await;
    let run_id = submitted["runId"].as_str().unwrap();
    let (status, run) = call(
        background_runs_router_with_state(state.clone()),
        "DELETE",
        &format!("/runs/{run_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(run["status"], "cancelled");
    let (status, _) =
        call(background_runs_router_with_state(state), "DELETE", "/runs/absent", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

fn cron_body(name: &str, expression: &str) -> Value {
    json!({ "name": name, "workflowId": "echo", "cronExpression": expression })
}

#[tokio::test]
async fn cron_jobs_are_created_read_paused_resumed_and_deleted() {
    let cron = CronState::new(BackgroundState::new().with_embedded_worker(false));
    let app = || cron_jobs_router_with_state(cron.clone());

    let (status, created) =
        call(app(), "POST", "/cron", Some(cron_body("nightly", "0 2 * * *"))).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["jobId"], "nightly");
    assert_eq!(created["timezone"], "UTC");
    assert_eq!(created["concurrencyPolicy"], "skip");
    assert!(created["nextFireAt"].is_string());

    let (status, _) = call(app(), "POST", "/cron", Some(cron_body("nightly", "0 3 * * *"))).await;
    assert_eq!(status, StatusCode::CONFLICT, "a name is unique");
    let (status, body) = call(app(), "POST", "/cron", Some(cron_body("bad", "not cron"))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid cron expression");

    let (status, job) = call(app(), "GET", "/cron/nightly", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(job["cronExpression"], "0 2 * * *");

    let (_, paused) =
        call(app(), "PATCH", "/cron/nightly", Some(json!({ "status": "paused" }))).await;
    assert_eq!(paused["status"], "paused");
    let (_, resumed) =
        call(app(), "PATCH", "/cron/nightly", Some(json!({ "status": "active" }))).await;
    assert_eq!(resumed["status"], "active");

    let (_, list) = call(app(), "GET", "/cron", None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (status, _) = call(app(), "DELETE", "/cron/nightly", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(app(), "GET", "/cron/nightly", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Resuming a paused job moves its next fire time past now, so occurrences that fell
/// inside the pause are skipped rather than replayed.
#[tokio::test]
async fn resuming_a_paused_job_skips_the_occurrences_missed_while_paused() {
    let cron = CronState::new(BackgroundState::new().with_embedded_worker(false));
    let app = || cron_jobs_router_with_state(cron.clone());
    call(app(), "POST", "/cron", Some(cron_body("minutely", "* * * * *"))).await;
    call(app(), "PATCH", "/cron/minutely", Some(json!({ "status": "paused" }))).await;
    let mut stale = cron.schedules.get("minutely").await.unwrap().unwrap();
    stale.next_fire_at = Some(chrono::Utc::now() - chrono::Duration::days(1));
    cron.schedules.upsert(stale).await.unwrap();

    call(app(), "PATCH", "/cron/minutely", Some(json!({ "status": "active" }))).await;
    let resumed = cron.schedules.get("minutely").await.unwrap().unwrap();
    assert!(resumed.next_fire_at.unwrap() > chrono::Utc::now());
}

/// The scheduler fires a job through the run store and the embedded worker runs it.
#[tokio::test]
async fn a_cron_job_fires_and_its_run_executes() {
    let background = BackgroundState::new().with_executor(Arc::new(registry()));
    let cron = CronState::new(background.clone());
    let (status, _) = call(
        cron_jobs_router_with_state(cron.clone()),
        "POST",
        "/cron",
        Some(json!({ "name": "each-second", "workflowId": "echo", "cronExpression": "* * * * * *", "input": { "name": "tick" } })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    background.ensure_worker_started();
    let scheduler = start_cron_scheduler(cron.clone());

    let runs = Arc::clone(background.run_store());
    let mut completed = None;
    for _ in 0..400 {
        let done = runs.list(RunFilter::default().states([RunState::Completed])).await.unwrap();
        if let Some(run) = done.into_iter().next() {
            completed = Some(run);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    scheduler.abort();
    background.shutdown_worker().await;
    let run = completed.expect("the cron job fired and its run completed");
    assert_eq!(run.result, Some(json!({ "echoed": "tick" })));
    assert!(run.idempotency_key.unwrap().starts_with("schedule:each-second:"));
    assert_eq!(run.concurrency_key.as_deref(), Some("cron:each-second"));
}
