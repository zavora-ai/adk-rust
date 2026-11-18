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
    assert_eq!(run["error"], "exploded; attempts exhausted (2 of 2)");
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

async fn post_with_key(app: Router, uri: &str, key: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("idempotency-key", key)
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// A jittered fire is enqueued at its fire time, keyed by it, and claimable only after the
/// schedule's offset; the schedule advances to the next fire time as without jitter.
#[tokio::test]
async fn a_jittered_fire_keeps_its_key_and_waits_out_its_offset() {
    use adk_core::schedule::{Jitter, MissedFirePolicy, Schedule, ScheduleSpec};
    use adk_core::{
        DurableRunStore, InMemoryRunStore, InMemoryScheduleStore, ManualClock, NewRun,
        ScheduleStore, firing_idempotency_key,
    };
    use adk_server::background::DurableScheduler;
    use chrono::TimeZone;

    let at = chrono::Utc.with_ymd_and_hms(2026, 11, 18, 9, 0, 0).unwrap();
    let clock = Arc::new(ManualClock::new(at));
    let schedules = Arc::new(InMemoryScheduleStore::new());
    let runs = Arc::new(InMemoryRunStore::new());
    let spec = ScheduleSpec::Every(Duration::from_secs(600));
    let template = NewRun::new("default", "ops", "system", "probe", json!({}));
    for id in ["probe-a", "probe-b"] {
        let schedule = Schedule::new(id, spec.clone(), template.clone(), MissedFirePolicy::Skip)
            .with_next_fire_at(at);
        schedules.upsert(schedule).await.unwrap();
    }
    let jitter = Jitter::new(50, Duration::from_secs(9 * 60));
    let governance = adk_core::GovernanceControl::new();
    let scheduler = DurableScheduler::new(schedules.clone(), runs.clone())
        .with_clock(clock.clone())
        .with_governance(governance.clone())
        .with_jitter(jitter);

    governance.freeze("maintenance");
    let frozen = scheduler.tick().await.unwrap();
    assert!(frozen.frozen && frozen.enqueued == 0, "a frozen scheduler fires nothing");
    governance.unfreeze();

    assert_eq!(scheduler.tick().await.unwrap().enqueued, 2);
    let mut offsets = Vec::new();
    for id in ["probe-a", "probe-b"] {
        let key = firing_idempotency_key(id, at);
        let run = runs
            .list(RunFilter::default())
            .await
            .unwrap()
            .into_iter()
            .find(|run| run.idempotency_key.as_deref() == Some(key.as_str()))
            .expect("the fire is keyed by its scheduled time");
        let offset = jitter.offset_in(&spec, id, at);
        assert!(offset < Duration::from_secs(300), "under half the gap: {offset:?}");
        let expected =
            (!offset.is_zero()).then(|| at + chrono::Duration::from_std(offset).unwrap());
        assert_eq!(run.not_before, expected, "{id} waits out its offset");
        offsets.push(offset);
        let next = schedules.get(id).await.unwrap().unwrap().next_fire_at;
        assert_eq!(
            next,
            Some(at + chrono::Duration::minutes(10)),
            "the schedule advances as planned"
        );
    }
    assert_ne!(offsets[0], offsets[1], "two schedules spread apart");
}

/// `POST /cron/{id}/run` enqueues the job's run now, keyed by the request, leaves the
/// schedule where it was, answers a retry with the same run, refuses while frozen and while
/// a `skip` job's run is active, and records each outcome.
#[tokio::test]
async fn a_cron_job_runs_now_without_moving_its_schedule() {
    use adk_core::audit_log::{AuditTrail, InMemoryAuditRecorder};

    let governance = adk_core::GovernanceControl::new();
    let background =
        BackgroundState::new().with_embedded_worker(false).with_governance(governance.clone());
    let recorder = Arc::new(InMemoryAuditRecorder::new());
    let cron = CronState::new(background.clone()).with_audit(AuditTrail::new(recorder.clone()));
    let app = || cron_jobs_router_with_state(cron.clone());
    let (status, _) = call(app(), "POST", "/cron", Some(cron_body("nightly", "0 2 * * *"))).await;
    assert_eq!(status, StatusCode::CREATED);
    let before = cron.schedules.get("nightly").await.unwrap().unwrap();

    let (status, body) = post_with_key(app(), "/cron/nightly/run", "r-1").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["idempotencyKey"], "schedule:nightly:manual:r-1");
    assert_eq!(body["existing"], false);
    let run_id = body["runId"].as_str().unwrap().to_string();
    let run = background.run_store().get(&run_id).await.unwrap().unwrap();
    assert_eq!(run.target, "echo");
    assert_eq!(run.concurrency_key.as_deref(), Some("cron:nightly"));

    let (status, again) = post_with_key(app(), "/cron/nightly/run", "r-1").await;
    assert_eq!((status, again["runId"].as_str()), (StatusCode::OK, Some(run_id.as_str())));
    assert_eq!(again["existing"], true);

    // The first run is still queued and the job's policy is skip.
    let (status, conflict) = post_with_key(app(), "/cron/nightly/run", "r-2").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(conflict["blockingRunId"], run_id.as_str());

    governance.freeze("incident");
    let (status, _) = post_with_key(app(), "/cron/nightly/run", "r-3").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    governance.unfreeze();

    let (status, _) = post_with_key(app(), "/cron/missing/run", "r-4").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let after = cron.schedules.get("nightly").await.unwrap().unwrap();
    assert_eq!(
        (after.next_fire_at, after.revision, after.paused),
        (before.next_fire_at, before.revision, before.paused),
        "a run now leaves the schedule as it was"
    );
    let outcomes: Vec<String> = recorder
        .records()
        .iter()
        .filter(|record| record.action == "schedule.run_now")
        .map(|record| record.ext["outcome"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(outcomes, ["enqueued", "existing", "refused_concurrency", "refused_frozen"]);
}

/// LIVE on Anthropic `claude-haiku-5-5`: a cron job's agent runs on a jittered fire, keyed by
/// its fire time and claimed no earlier than its offset, then again on `POST /cron/{id}/run`.
///
/// ```text
/// cargo test -p adk-server --features background --test background_execution_tests live_ -- --ignored --nocapture
/// ```
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: needs ANTHROPIC_API_KEY"]
async fn live_a_cron_agent_runs_on_a_jittered_fire_and_on_run_now() {
    use adk_core::schedule::{Jitter, ScheduleSpec};
    use adk_core::{Llm, RunRecord};

    let _ = dotenvy::from_path(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.env"));
    let key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY");
    let model: Arc<dyn Llm> = Arc::new(
        adk_model::anthropic::AnthropicClient::new(adk_model::anthropic::AnthropicConfig::new(
            key,
            "claude-haiku-5-5",
        ))
        .unwrap(),
    );
    let agent = adk_agent::LlmAgentBuilder::new("report_agent")
        .instruction(
            "You run on a schedule. Reply with one short sentence confirming the report ran.",
        )
        .model(model)
        .build()
        .unwrap();
    let runner = Arc::new(
        adk_runner::Runner::builder()
            .app_name("reports")
            .agent(Arc::new(agent))
            .session_service(Arc::new(adk_session::InMemorySessionService::new()))
            .build()
            .unwrap(),
    );
    let background = BackgroundState::new().with_agent("report_agent", runner);
    let jitter = Jitter::new(50, Duration::from_secs(9 * 60));
    let cron = CronState::new(background.clone()).with_jitter(jitter);
    let every = "*/20 * * * * *";
    let (status, _) = call(
        cron_jobs_router_with_state(cron.clone()),
        "POST",
        "/cron",
        Some(json!({ "name": "report", "workflowId": "report_agent", "cronExpression": every, "input": { "text": "Run the report." } })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    background.ensure_worker_started();
    let scheduler = start_cron_scheduler(cron.clone());

    let completed = |prefix: String| {
        let runs = Arc::clone(background.run_store());
        async move {
            for _ in 0..360 {
                let done =
                    runs.list(RunFilter::default().states([RunState::Completed])).await.unwrap();
                if let Some(run) = done.into_iter().find(|run: &RunRecord| {
                    run.idempotency_key.as_deref().is_some_and(|key| key.starts_with(&prefix))
                }) {
                    return run;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            panic!("no completed run keyed {prefix}");
        }
    };
    // The fire as it waits in the queue, before a worker claims it and clears `not_before`.
    let queued = {
        let runs = Arc::clone(background.run_store());
        let mut seen = None;
        for _ in 0..1200 {
            let all = runs.list(RunFilter::default()).await.unwrap();
            if let Some(run) = all.into_iter().find(|run| {
                run.idempotency_key
                    .as_deref()
                    .is_some_and(|key| key.starts_with("schedule:report:2"))
            }) {
                seen = Some(run);
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        seen.expect("the job fired")
    };
    let key = queued.idempotency_key.clone().unwrap();
    let at: chrono::DateTime<chrono::Utc> = key["schedule:report:".len()..].parse().unwrap();
    let spec = ScheduleSpec::Cron { expr: every.into(), timezone: "UTC".into() };
    let offset = jitter.offset_in(&spec, "report", at);
    let release = at + chrono::Duration::from_std(offset).unwrap();
    println!(
        "fire {at}: offset {offset:?}, enqueued {} as {:?} with not_before {:?}",
        queued.created_at, queued.state, queued.not_before
    );
    assert!(offset < Duration::from_secs(10), "under half the 20 s gap");
    if queued.state == RunState::Queued && !offset.is_zero() {
        assert_eq!(queued.not_before, Some(release), "claimable only after its offset");
    }
    let fired = completed(key.clone()).await;
    println!("fire {at} completed at {}: {:?}", fired.updated_at, fired.result);
    assert_eq!(fired.idempotency_key.as_deref(), Some(key.as_str()), "keyed by the fire time");
    assert!(fired.updated_at >= release, "ran no earlier than its offset");

    let (status, body) =
        post_with_key(cron_jobs_router_with_state(cron.clone()), "/cron/report/run", "live-1")
            .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let manual = completed("schedule:report:manual:live-1".to_string()).await;
    println!("run now {} result {:?}", manual.run_id, manual.result);
    scheduler.abort();
    background.shutdown_worker().await;
}
