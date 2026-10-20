//! Background runs belong to the authenticated caller (spec 3.1 §8.2 prerequisite).
//!
//! With a `RequestContextExtractor`, a caller reads, lists, cancels, and wakes only their
//! own runs, and their idempotency and concurrency keys never reach another owner's run.

#![cfg(feature = "background")]

use std::collections::BTreeSet;
use std::sync::Arc;

use adk_core::{Agent, EventStream, InvocationContext, Result as AdkResult, SingleAgentLoader};
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::background::{BackgroundState, WorkflowRegistry};
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
    ServerBuilder::new(config).with_background_runs(background).build()
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

#[tokio::test]
async fn without_an_extractor_runs_keep_todays_open_behaviour() {
    let app = app(false);
    let (status, created) = send(
        &app,
        "POST",
        "/api/runs",
        None,
        Some(json!({ "target": "report", "input": {}, "userId": "alice", "idempotencyKey": "k" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = run_id(&created);

    let (status, _) = send(&app, "GET", &format!("/api/runs/{id}"), None, None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = send(&app, "GET", "/api/runs", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);

    let (status, again) = send(
        &app,
        "POST",
        "/api/runs",
        None,
        Some(json!({ "target": "report", "input": {}, "userId": "bob", "idempotencyKey": "k" })),
    )
    .await;
    assert_eq!((status, run_id(&again)), (StatusCode::OK, id), "keys stay global, as before");
}
