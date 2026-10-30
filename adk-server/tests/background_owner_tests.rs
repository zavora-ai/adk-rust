//! Background runs belong to the authenticated caller (spec 3.1 §8.2 prerequisite).
//!
//! With a `RequestContextExtractor`, a caller reads, lists, cancels, and wakes only their
//! own runs, and their idempotency and concurrency keys never reach another owner's run.
//! Without one the routes run in open mode: every client reaches every run, an idempotency
//! key is scoped to the user the request names, and the server warns when the routes mount.

#![cfg(feature = "background")]

use std::collections::BTreeSet;
use std::sync::Arc;

use adk_core::{Agent, EventStream, InvocationContext, Result as AdkResult, SingleAgentLoader};
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::background::{BackgroundState, CronState, WorkflowRegistry};
use adk_server::{ServerBuilder, ServerConfig};
use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

struct IdleAgent;

#[async_trait]
impl Agent for IdleAgent {
    fn name(&self) -> &str {
        "idle_agent"
    }
    fn description(&self) -> &str {
        "never runs"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }
    async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

/// Authenticates the user named in `x-user`.
struct HeaderUser;

#[async_trait]
impl RequestContextExtractor for HeaderUser {
    async fn extract(
        &self,
        parts: &axum::http::request::Parts,
    ) -> Result<adk_core::RequestContext, RequestContextError> {
        let user = parts
            .headers
            .get("x-user")
            .and_then(|value| value.to_str().ok())
            .ok_or(RequestContextError::MissingAuth)?;
        Ok(adk_core::RequestContext {
            user_id: user.to_string(),
            scopes: vec![],
            metadata: Default::default(),
        })
    }
}

/// A server whose runs stay queued: no worker claims them, so each test sees the state its
/// own requests leave.
fn app(authenticated: bool) -> Router {
    app_with_state(authenticated).0
}

/// [`app`] with cron jobs, and the stores behind it.
fn app_with_state(authenticated: bool) -> (Router, BackgroundState, CronState) {
    let loader = Arc::new(SingleAgentLoader::new(Arc::new(IdleAgent)));
    let mut config =
        ServerConfig::new(loader, Arc::new(adk_session::InMemorySessionService::new()));
    if authenticated {
        config = config.with_request_context(Arc::new(HeaderUser));
    }
    let workflows =
        WorkflowRegistry::new().register("report", |_input, _cancel| async { Ok(json!({})) });
    let background =
        BackgroundState::new().with_executor(Arc::new(workflows)).with_embedded_worker(false);
    let cron = CronState::new(background.clone());
    let app = ServerBuilder::new(config)
        .with_background_runs(background.clone())
        .with_cron_jobs(cron.clone())
        .build();
    (app, background, cron)
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    user: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(user) = user {
        builder = builder.header("x-user", user);
    }
    let request = match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn submit(app: &Router, user: &str, extra: Value) -> (StatusCode, Value) {
    let mut body = json!({ "target": "report", "input": {} });
    body.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    send(app, "POST", "/api/runs", Some(user), Some(body)).await
}

fn run_id(response: &Value) -> String {
    response["runId"].as_str().expect("a run id").to_string()
}

#[tokio::test]
async fn another_owners_run_cannot_be_read_cancelled_or_woken() {
    let app = app(true);
    let (status, created) = submit(&app, "alice", json!({})).await;
    assert_eq!(status, StatusCode::CREATED);
    let id = run_id(&created);
    let run = format!("/api/runs/{id}");
    let wake = format!("/api/runs/{id}/wake");

    for (method, uri) in [("GET", &run), ("DELETE", &run), ("POST", &wake)] {
        let (status, body) = send(&app, method, uri, Some("bob"), None).await;
        assert_eq!(
            (status, body),
            (StatusCode::NOT_FOUND, json!({ "error": "run not found" })),
            "bob's {method} {uri} must look like an unknown run"
        );
    }

    let (status, body) = send(&app, "GET", &run, Some("alice"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "queued", "bob's DELETE did not cancel alice's run");
    assert_eq!(body["cancelRequested"], false);

    let (status, _) = send(&app, "POST", &wake, Some("alice"), None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = send(&app, "DELETE", &run, Some("alice"), None).await;
    assert_eq!((status, &body["status"]), (StatusCode::OK, &json!("cancelled")));
}

#[tokio::test]
async fn the_run_list_shows_only_the_callers_runs() {
    let app = app(true);
    let mut alice = BTreeSet::new();
    for _ in 0..2 {
        alice.insert(run_id(&submit(&app, "alice", json!({})).await.1));
    }
    let bob = BTreeSet::from([run_id(&submit(&app, "bob", json!({})).await.1)]);

    let listed =
        |body: Value| -> BTreeSet<String> { body.as_array().unwrap().iter().map(run_id).collect() };
    let (status, body) = send(&app, "GET", "/api/runs", Some("alice"), None).await;
    assert_eq!((status, listed(body)), (StatusCode::OK, alice));
    let (status, body) = send(&app, "GET", "/api/runs", Some("bob"), None).await;
    assert_eq!((status, listed(body)), (StatusCode::OK, bob));
    let (status, body) = send(&app, "GET", "/api/runs", Some("carol"), None).await;
    assert_eq!((status, body), (StatusCode::OK, json!([])));
}

#[tokio::test]
async fn an_idempotency_key_is_scoped_to_its_caller() {
    let app = app(true);
    let key = json!({ "idempotencyKey": "webhook:evt_1" });

    let (status, alice) = submit(&app, "alice", key.clone()).await;
    assert_eq!((status, &alice["existing"]), (StatusCode::CREATED, &json!(false)));
    let (status, bob) = submit(&app, "bob", key.clone()).await;
    assert_eq!(
        (status, &bob["existing"]),
        (StatusCode::CREATED, &json!(false)),
        "bob's key is not alice's: a new run, not hers"
    );
    assert_ne!(run_id(&alice), run_id(&bob));

    let (status, again) = submit(&app, "alice", key).await;
    assert_eq!((status, &again["existing"]), (StatusCode::OK, &json!(true)));
    assert_eq!(run_id(&again), run_id(&alice), "alice's own retry still deduplicates");

    let (status, _) = submit(&app, "alice", json!({ "idempotencyKey": "" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "an empty key is still refused");
}

#[tokio::test]
async fn a_concurrency_key_cannot_reject_or_interrupt_another_owners_run() {
    let app = app(true);
    let (_, alice) = submit(&app, "alice", json!({ "concurrencyKey": "thread-42" })).await;
    let alice_run = format!("/api/runs/{}", run_id(&alice));

    let (status, rejected) =
        submit(&app, "bob", json!({ "concurrencyKey": "thread-42", "onConflict": "reject" })).await;
    assert_eq!(status, StatusCode::CREATED, "bob is not blocked by alice: {rejected}");
    let (status, _) =
        submit(&app, "carol", json!({ "concurrencyKey": "thread-42", "onConflict": "interrupt" }))
            .await;
    assert_eq!(status, StatusCode::CREATED);

    let (_, body) = send(&app, "GET", &alice_run, Some("alice"), None).await;
    assert_eq!(body["status"], "queued", "carol's interrupt did not cancel alice's run");

    let (status, body) =
        submit(&app, "alice", json!({ "concurrencyKey": "thread-42", "onConflict": "reject" }))
            .await;
    assert_eq!(status, StatusCode::CONFLICT, "alice's own key still serializes her runs");
    assert_eq!(body["blockingRunId"], json!(run_id(&alice)));
}

// ─── Open mode: no extractor ─────────────────────────────────────────────────

/// Submits a run in open mode, naming `user` in the body when given.
async fn submit_open(app: &Router, user: Option<&str>, extra: Value) -> (StatusCode, Value) {
    let mut body = json!({ "target": "report", "input": {} });
    if let Some(user) = user {
        body["userId"] = json!(user);
    }
    body.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    send(app, "POST", "/api/runs", None, Some(body)).await
}

#[tokio::test]
async fn without_an_extractor_every_client_reads_and_lists_every_run() {
    let app = app(false);
    let (status, created) = submit_open(&app, Some("alice"), json!({})).await;
    assert_eq!(status, StatusCode::CREATED);
    let id = run_id(&created);

    let (status, _) = send(&app, "GET", &format!("/api/runs/{id}"), None, None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = send(&app, "GET", "/api/runs", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn without_an_extractor_any_client_wakes_and_cancels_any_run() {
    let app = app(false);
    let (_, created) = submit_open(&app, Some("alice"), json!({})).await;
    let run = format!("/api/runs/{}", run_id(&created));
    let wake = format!("{run}/wake");

    let (status, body) = send(&app, "POST", &wake, None, None).await;
    assert_eq!((status, &body["status"]), (StatusCode::OK, &json!("queued")), "{body}");
    let (status, body) = send(&app, "DELETE", &run, None, None).await;
    assert_eq!((status, &body["status"]), (StatusCode::OK, &json!("cancelled")), "{body}");
    let (_, body) = send(&app, "GET", &run, None, None).await;
    assert_eq!(body["status"], "cancelled");

    for (method, uri) in [("DELETE", "/api/runs/absent"), ("POST", "/api/runs/absent/wake")] {
        let (status, body) = send(&app, method, uri, None, None).await;
        assert_eq!(
            (status, body),
            (StatusCode::NOT_FOUND, json!({ "error": "run not found" })),
            "{method} {uri}"
        );
    }
}

#[tokio::test]
async fn without_an_extractor_an_idempotency_key_is_scoped_to_the_named_user() {
    let app = app(false);
    let key = json!({ "idempotencyKey": "webhook:evt_1" });

    let (status, alice) = submit_open(&app, Some("alice"), key.clone()).await;
    assert_eq!((status, &alice["existing"]), (StatusCode::CREATED, &json!(false)));
    let (status, bob) = submit_open(&app, Some("bob"), key.clone()).await;
    assert_eq!(
        (status, &bob["existing"]),
        (StatusCode::CREATED, &json!(false)),
        "bob's key is not alice's: a new run, not hers"
    );
    assert_ne!(run_id(&alice), run_id(&bob));

    let (status, again) = submit_open(&app, Some("alice"), key.clone()).await;
    assert_eq!((status, run_id(&again)), (StatusCode::OK, run_id(&alice)), "alice's retry");

    // Requests naming no user are all `anonymous`, so they share one user's keys.
    let (status, first) = submit_open(&app, None, key.clone()).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, second) = submit_open(&app, None, key).await;
    assert_eq!((status, run_id(&second)), (StatusCode::OK, run_id(&first)));

    let (status, _) = submit_open(&app, Some("alice"), json!({ "idempotencyKey": "" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "an empty key is still refused");
}

#[tokio::test]
async fn without_an_extractor_a_key_cannot_take_a_schedule_firing() {
    let (app, background, cron) = app_with_state(false);
    let (status, _) = send(&app, "POST", "/api/cron", None, Some(nightly())).await;
    assert_eq!(status, StatusCode::CREATED);
    let schedule = cron.schedules.get("nightly").await.unwrap().expect("stored under its name");
    let fire_at = schedule.next_fire_at.expect("a next fire time");

    let firing_key = adk_core::schedule::firing_idempotency_key("nightly", fire_at);
    let (status, _) =
        submit_open(&app, Some("cron"), json!({ "idempotencyKey": firing_key })).await;
    assert_eq!(status, StatusCode::CREATED);

    let fired = background.run_store().enqueue(schedule.run_for(fire_at)).await.unwrap();
    assert!(
        matches!(fired, adk_core::Enqueued::Created(_)),
        "the firing is its own run, not the client's: {fired:?}"
    );
}

/// Collects formatted tracing output.
#[derive(Clone, Default)]
struct CapturedLogs(Arc<std::sync::Mutex<Vec<u8>>>);

impl CapturedLogs {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl std::io::Write for CapturedLogs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
    type Writer = CapturedLogs;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// The open-mode warnings `work` logs, one entry per warning.
fn open_mode_warnings(work: impl FnOnce()) -> Vec<String> {
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .finish();
    tracing::subscriber::with_default(subscriber, work);
    logs.text().lines().filter(|line| line.contains("open mode")).map(str::to_string).collect()
}

#[tokio::test]
async fn open_mode_is_announced_when_the_routes_mount_not_on_a_request() {
    let mut open = None;
    let warnings = open_mode_warnings(|| open = Some(app(false)));
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("WARN"), "{warnings:?}");
    assert!(warnings[0].contains("server.routes=\"/api/runs, /api/cron\""), "{warnings:?}");

    let warnings = open_mode_warnings(|| drop(app(true)));
    assert_eq!(warnings, Vec::<String>::new(), "an extractor authenticates every caller");

    let warnings = open_mode_warnings(|| {
        let background = BackgroundState::new().with_embedded_worker(false);
        drop(adk_server::background::background_runs_router_with_state(background.clone()));
        drop(adk_server::background::cron_jobs_router_with_state(CronState::new(background)));
    });
    assert_eq!(warnings.len(), 2, "each standalone router warns: {warnings:?}");

    let open = open.unwrap();
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt().with_writer(logs.clone()).with_ansi(false).finish();
    let guard = tracing::subscriber::set_default(subscriber);
    let (status, _) = submit_open(&open, Some("alice"), json!({})).await;
    drop(guard);
    assert_eq!(status, StatusCode::CREATED);
    let logs = logs.text();
    assert!(
        !logs.contains("open mode") && !logs.contains("authenticated caller"),
        "no warning per request: {logs}"
    );
}

// ─── Authenticated runs and cron owners (spec 3.1 §8.2, §9.2 I6) ──────────────

#[tokio::test]
async fn a_run_an_authenticated_caller_submits_is_stored_as_authenticated() {
    let (app, background, _) = app_with_state(true);
    let (_, created) = submit(&app, "alice", json!({})).await;
    let stored = background.run_store().get(&run_id(&created)).await.unwrap().unwrap();
    assert_eq!((stored.user_id.as_str(), stored.authenticated), ("alice", true));

    let (app, background, _) = app_with_state(false);
    let body = json!({ "target": "report", "input": {}, "userId": "mallory" });
    let (_, created) = send(&app, "POST", "/api/runs", None, Some(body)).await;
    let stored = background.run_store().get(&run_id(&created)).await.unwrap().unwrap();
    assert_eq!((stored.user_id.as_str(), stored.authenticated), ("mallory", false));
}

fn nightly() -> Value {
    json!({ "name": "nightly", "workflowId": "report", "cronExpression": "0 0 3 * * *" })
}

#[tokio::test]
async fn another_owners_cron_job_cannot_be_listed_read_changed_or_deleted() {
    let (app, _, cron) = app_with_state(true);
    let (status, created) = send(&app, "POST", "/api/cron", Some("alice"), Some(nightly())).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(
        (created["jobId"].as_str(), created["name"].as_str()),
        (Some("nightly"), Some("nightly"))
    );

    let (status, listed) = send(&app, "GET", "/api/cron", Some("bob"), None).await;
    assert_eq!((status, listed), (StatusCode::OK, json!([])), "bob lists none of alice's jobs");
    let pause = json!({ "status": "paused" });
    for (method, body) in [("GET", None), ("PATCH", Some(pause.clone())), ("DELETE", None)] {
        let (status, _) = send(&app, method, "/api/cron/nightly", Some("bob"), body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "bob's {method} must look like an unknown job");
    }
    let (status, job) = send(&app, "GET", "/api/cron/nightly", Some("alice"), None).await;
    assert_eq!((status, job["status"].as_str()), (StatusCode::OK, Some("active")), "untouched");

    // The job runs for alice, as the identity the server authenticated.
    let schedules = cron.schedules.list().await.unwrap();
    assert_eq!(schedules.len(), 1);
    assert_eq!(
        (schedules[0].run.user_id.as_str(), schedules[0].run.authenticated),
        ("alice", true)
    );

    let (status, _) = send(&app, "PATCH", "/api/cron/nightly", Some("alice"), Some(pause)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(&app, "DELETE", "/api/cron/nightly", Some("alice"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn two_owners_may_each_have_a_cron_job_of_the_same_name() {
    let (app, _, cron) = app_with_state(true);
    for owner in ["alice", "bob"] {
        let (status, job) = send(&app, "POST", "/api/cron", Some(owner), Some(nightly())).await;
        assert_eq!(status, StatusCode::CREATED, "{owner}: {job}");
    }
    let (status, _) = send(&app, "POST", "/api/cron", Some("alice"), Some(nightly())).await;
    assert_eq!(status, StatusCode::CONFLICT, "an owner's own name is still unique");

    let (_, listed) = send(&app, "GET", "/api/cron", Some("bob"), None).await;
    assert_eq!(listed.as_array().map(Vec::len), Some(1));
    assert_eq!(listed[0]["name"], "nightly");
    let mut owners: Vec<String> =
        cron.schedules.list().await.unwrap().into_iter().map(|s| s.run.user_id).collect();
    owners.sort();
    assert_eq!(owners, ["alice", "bob"]);
}

#[tokio::test]
async fn without_an_extractor_cron_jobs_keep_their_names_and_run_for_cron() {
    let (app, _, cron) = app_with_state(false);
    let (status, _) = send(&app, "POST", "/api/cron", None, Some(nightly())).await;
    assert_eq!(status, StatusCode::CREATED);
    let schedule = cron.schedules.get("nightly").await.unwrap().expect("stored under its name");
    assert_eq!((schedule.run.user_id.as_str(), schedule.run.authenticated), ("cron", false));
}
