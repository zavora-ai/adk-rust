//! Background runs must actually run, and a cron occurrence must be claimed once.
//!
//! Three defects motivated these tests:
//!
//! 1. `BackgroundRunner::run_with_timeout` received neither the workflow ID nor the
//!    input. It checked cancellation and returned `Completed` with an empty object, so a
//!    client got a completed status for work that never ran, and retry could never be
//!    exercised because the placeholder had no way to fail.
//! 2. Under the `Queue` concurrency policy the scheduler advanced `last_execution` only
//!    when a run *started*, so an occurrence waiting behind an active run stayed due and
//!    was enqueued again on every one-second poll. When the active run finished, its
//!    monitor started one queued run without creating a monitor for it, leaving
//!    `active_run_count` permanently nonzero and the queue stalled.
//! 3. The module documented `GET /cron/{job_id}`; only PATCH and DELETE were mounted.

#![cfg(feature = "background")]

use adk_server::background::{
    BackgroundState, CronState, RunStatus, WorkflowRegistry, background_runs_router_with_state,
    cron_jobs_router_with_state,
};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tower::ServiceExt;

/// Polls a run until it reaches a terminal status, or gives up.
async fn await_terminal(state: &BackgroundState, run_id: &str) -> RunStatus {
    for _ in 0..200 {
        if let Some(run) = state.store.get(run_id).await {
            match run.status {
                RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled => {
                    return run.status;
                }
                _ => {}
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("run {run_id} never reached a terminal status");
}

// ── Background runs execute real work ──────────────────────────────────

#[tokio::test]
async fn a_run_executes_its_workflow_and_returns_the_output() {
    let registry = WorkflowRegistry::new().register("echo", |input, _cancel| async move {
        let name = input.get("name").cloned().unwrap_or(Value::Null);
        Ok(json!({ "echoed": name }))
    });
    let state = BackgroundState::new().with_executor(Arc::new(registry));

    let response = background_runs_router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/runs")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "workflowId": "echo", "input": { "name": "ada" } }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let submitted: Value = serde_json::from_slice(&body).unwrap();
    let run_id = submitted["runId"].as_str().unwrap().to_string();

    assert_eq!(
        await_terminal(&state, &run_id).await,
        RunStatus::Completed,
        "the run must complete by executing the workflow"
    );
    let run = state.store.get(&run_id).await.unwrap();
    assert_eq!(
        run.result,
        Some(json!({ "echoed": "ada" })),
        "the workflow's output must be returned, and its input must have reached it"
    );
}

#[tokio::test]
async fn a_failing_workflow_fails_the_run() {
    let registry = WorkflowRegistry::new()
        .register("boom", |_input, _cancel| async move { Err("exploded".to_string()) });
    let state = BackgroundState::new().with_executor(Arc::new(registry));

    let run_id = submit(&state, json!({ "workflowId": "boom", "input": {} })).await;

    assert_eq!(await_terminal(&state, &run_id).await, RunStatus::Failed);
    let run = state.store.get(&run_id).await.unwrap();
    assert_eq!(run.error.as_deref(), Some("exploded"), "the failure reason must be recorded");
}

#[tokio::test]
async fn a_failing_workflow_consumes_its_retry_budget() {
    // The placeholder could never fail, so the retry path was never exercised.
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = attempts.clone();
    let registry = WorkflowRegistry::new().register("flaky", move |_input, _cancel| {
        let counter = counter.clone();
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Err("still failing".to_string())
        }
    });
    let state = BackgroundState::new().with_executor(Arc::new(registry));

    let run_id =
        submit(&state, json!({ "workflowId": "flaky", "input": {}, "maxRetries": 2 })).await;
    assert_eq!(await_terminal(&state, &run_id).await, RunStatus::Failed);

    assert_eq!(
        attempts.load(Ordering::SeqCst),
        3,
        "the workflow must run once plus twice more for the retry budget"
    );
}

#[tokio::test]
async fn an_unknown_workflow_is_rejected_instead_of_queued() {
    let state = BackgroundState::new().with_executor(Arc::new(WorkflowRegistry::new()));

    let response = background_runs_router_with_state(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/runs")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "workflowId": "nope", "input": {} }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "an unknown workflow must be refused rather than accepted and reported on"
    );
}

#[tokio::test]
async fn a_run_without_an_executor_fails_rather_than_reporting_success() {
    // This is the original defect in its plainest form.
    let state = BackgroundState::new();
    let run_id = submit(&state, json!({ "workflowId": "anything", "input": {} })).await;

    assert_eq!(
        await_terminal(&state, &run_id).await,
        RunStatus::Failed,
        "with nothing able to run the workflow, the run must not report completion"
    );
}

/// Submits a run and returns its ID.
async fn submit(state: &BackgroundState, body: Value) -> String {
    let response = background_runs_router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/runs")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let submitted: Value = serde_json::from_slice(&bytes).unwrap();
    submitted["runId"].as_str().unwrap().to_string()
}

// ── A cron occurrence is claimed once ──────────────────────────────────

#[tokio::test]
async fn an_occurrence_can_be_claimed_only_once() {
    let state = CronState::new(BackgroundState::new());
    let job_id = create_job(&state, "* * * * * *").await;

    // Every-second schedule, so the job is due almost immediately.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let due = state.cron_store.due_occurrences().await;
    let (_, occurrence) =
        due.iter().find(|(job, _)| job.job_id == job_id).expect("job must be due");

    assert!(state.cron_store.claim_occurrence(&job_id, *occurrence).await, "the first claim wins");
    assert!(
        !state.cron_store.claim_occurrence(&job_id, *occurrence).await,
        "the same occurrence must not be claimable twice; that is what turned one \
         schedule point into a queue entry per poll"
    );
}

#[tokio::test]
async fn a_claimed_occurrence_is_no_longer_due() {
    let state = CronState::new(BackgroundState::new());
    let job_id = create_job(&state, "0 0 * * * *").await;

    tokio::time::sleep(Duration::from_millis(50)).await;
    if let Some((_, occurrence)) =
        state.cron_store.due_occurrences().await.into_iter().find(|(j, _)| j.job_id == job_id)
    {
        state.cron_store.claim_occurrence(&job_id, occurrence).await;
        let still_due = state
            .cron_store
            .due_occurrences()
            .await
            .into_iter()
            .any(|(j, o)| j.job_id == job_id && o == occurrence);
        assert!(!still_due, "a claimed occurrence must not be offered again");
    }
}

#[tokio::test]
async fn claiming_an_older_occurrence_is_refused() {
    let state = CronState::new(BackgroundState::new());
    let job_id = create_job(&state, "* * * * * *").await;

    let now = Utc::now();
    assert!(state.cron_store.claim_occurrence(&job_id, now).await);
    assert!(
        !state.cron_store.claim_occurrence(&job_id, now - chrono::Duration::seconds(30)).await,
        "scheduling state must not move backwards"
    );
}

// ── The documented cron detail route exists ────────────────────────────

#[tokio::test]
async fn the_cron_detail_route_is_mounted() {
    let state = CronState::new(BackgroundState::new());
    let job_id = create_job(&state, "0 * * * * *").await;

    let found = cron_jobs_router_with_state(state.clone())
        .oneshot(Request::builder().uri(format!("/cron/{job_id}")).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(found.status(), StatusCode::OK, "GET /cron/{{job_id}} is documented and must exist");

    let bytes = axum::body::to_bytes(found.into_body(), usize::MAX).await.unwrap();
    let job: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(job["jobId"].as_str(), Some(job_id.as_str()));

    let missing = cron_jobs_router_with_state(state)
        .oneshot(Request::builder().uri("/cron/absent").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

/// Creates a cron job through the router and returns its ID.
async fn create_job(state: &CronState, expression: &str) -> String {
    let response = cron_jobs_router_with_state(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/cron")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "scheduled job",
                        "workflowId": "scheduled",
                        "cronExpression": expression,
                        "input": {}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let created: Value = serde_json::from_slice(&bytes).unwrap();
    created["jobId"].as_str().unwrap().to_string()
}
