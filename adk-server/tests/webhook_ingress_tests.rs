//! `POST /hooks/{name}`: signed deliveries enqueue durable runs exactly once.
//!
//! The durable worker is not part of this crate yet, so [`run_queued`] stands in for
//! it: a minimal claim loop that runs each claimed agent run through a `Runner` and
//! finishes it. Integration point: replace it with the server's embedded worker over
//! the same `DurableRunStore`.

#![cfg(feature = "webhook-ingress")]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_core::run_store::{
    DurableRunStore, InMemoryRunStore, NewRun, OnConflict, RunFilter, RunOutcome, RunState,
};
use adk_core::webhook::{WebhookSecret, WebhookSigner, WebhookVerifier};
use adk_core::{
    Agent, Content, Event, EventStream, GovernanceControl, InvocationContext, ManualClock,
    Result as AdkResult, SingleAgentLoader,
};
use adk_runner::Runner;
use adk_server::webhooks::{WebhookHook, WebhookIngress};
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::StreamExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// Replies with the text of the user turn it received.
struct EchoAgent {
    runs: Arc<AtomicUsize>,
}

#[async_trait]
impl Agent for EchoAgent {
    fn name(&self) -> &str {
        "triage_agent"
    }
    fn description(&self) -> &str {
        "echoes the wake-up"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }
    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        let text = ctx
            .user_content()
            .parts
            .iter()
            .filter_map(|part| part.text())
            .collect::<Vec<_>>()
            .join("");
        let mut event = Event::new(ctx.invocation_id());
        event.author = "triage_agent".to_string();
        event.llm_response.content =
            Some(Content::new("model").with_text(format!("handled: {text}")));
        Ok(Box::pin(futures::stream::iter([Ok(event)])))
    }
}

fn secret(text: &str) -> WebhookSecret {
    WebhookSecret::from_bytes(text.as_bytes().to_vec()).unwrap()
}

struct Fixture {
    app: Router,
    store: Arc<InMemoryRunStore>,
    governance: GovernanceControl,
    sessions: Arc<InMemorySessionService>,
    agent: Arc<EchoAgent>,
}

fn fixture_with(hook: impl FnOnce(WebhookHook) -> WebhookHook) -> Fixture {
    let store = Arc::new(InMemoryRunStore::new());
    let governance = GovernanceControl::new();
    let sessions = Arc::new(InMemorySessionService::new());
    let agent = Arc::new(EchoAgent { runs: Arc::new(AtomicUsize::new(0)) });
    let config = ServerConfig::new(
        Arc::new(SingleAgentLoader::new(agent.clone() as Arc<dyn Agent>)),
        sessions.clone() as Arc<dyn SessionService>,
    )
    .with_governance(governance.clone());
    // The receiver holds the current and the next key, so the sender can rotate.
    let verifier = WebhookVerifier::new(secret("current-key")).with_secret(secret("next-key"));
    let ingress = WebhookIngress::new(store.clone() as Arc<dyn DurableRunStore>)
        .with_retry_after(Duration::from_secs(7))
        .with_hook(hook(
            WebhookHook::new("github", verifier, "triage", "triage_agent")
                .with_input_template(json!({"text": "issue #{{/payload/issue/number}}"})),
        ));
    let app = ServerBuilder::new(config).with_webhook_ingress(ingress).build();
    Fixture { app, store, governance, sessions, agent }
}

fn fixture() -> Fixture {
    fixture_with(|hook| hook)
}

fn signed_request(path: &str, signer: &WebhookSigner, id: &str, body: &Value) -> Request<Body> {
    let body = serde_json::to_vec(body).unwrap();
    let signed = signer.sign_now(id, &body);
    let mut request = Request::post(path).header("content-type", "application/json");
    for (name, value) in signed.header_pairs() {
        request = request.header(name, value);
    }
    request.body(Body::from(body)).unwrap()
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, Value, Option<String>) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null), retry_after)
}

fn issue(number: u64) -> Value {
    json!({"issue": {"number": number}, "repository": {"id": 99}})
}

/// Stand-in for the durable worker: claims and runs every queued run once.
async fn run_queued(fixture: &Fixture) -> usize {
    let runner = Runner::builder()
        .app_name("triage")
        .agent(fixture.agent.clone() as Arc<dyn Agent>)
        .session_service(fixture.sessions.clone() as Arc<dyn SessionService>)
        .build()
        .unwrap();
    let mut finished = 0;
    while let Some(claimed) =
        fixture.store.claim("default", "test-worker", Duration::from_secs(30)).await.unwrap()
    {
        let record = &claimed.record;
        let session_id = record.session_id.clone().unwrap_or_else(|| record.run_id.clone());
        fixture
            .sessions
            .create(CreateRequest {
                app_name: record.app_name.clone(),
                user_id: record.user_id.clone(),
                session_id: Some(session_id.clone()),
                state: HashMap::new(),
            })
            .await
            .unwrap();
        let text = record.input["text"].as_str().unwrap_or_default().to_string();
        let mut stream = runner
            .run_str(&record.user_id, &session_id, Content::new("user").with_text(text))
            .await
            .unwrap();
        let mut answer = String::new();
        while let Some(event) = stream.next().await {
            if let Some(content) = event.unwrap().llm_response.content {
                answer.extend(content.parts.iter().filter_map(|part| part.text()));
            }
        }
        fixture
            .store
            .finish(&record.run_id, claimed.epoch, RunOutcome::Completed(json!({"text": answer})))
            .await
            .unwrap();
        finished += 1;
    }
    finished
}

#[tokio::test]
async fn a_signed_webhook_wakes_one_run_that_completes() {
    let fixture = fixture();
    let signer = WebhookSigner::new(secret("current-key"));
    let (status, body, _) =
        send(&fixture.app, signed_request("/hooks/github", &signer, "msg_1", &issue(42))).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!((body["state"].clone(), body["duplicate"].clone()), (json!("queued"), json!(false)));
    let run_id = body["runId"].as_str().unwrap().to_string();

    let record = fixture.store.get(&run_id).await.unwrap().unwrap();
    assert_eq!(
        (
            record.app_name.as_str(),
            record.user_id.as_str(),
            record.target.as_str(),
            record.idempotency_key.as_deref(),
            &record.input
        ),
        (
            "triage",
            "webhook:github",
            "triage_agent",
            Some("hook:github:msg_1"),
            &json!({"text": "issue #42"})
        )
    );

    assert_eq!(run_queued(&fixture).await, 1);
    let done = fixture.store.get(&run_id).await.unwrap().unwrap();
    assert_eq!(done.state, RunState::Completed);
    assert_eq!(done.result, Some(json!({"text": "handled: issue #42"})));
    assert_eq!(fixture.agent.runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_redelivered_webhook_returns_the_same_run_and_never_runs_twice() {
    let fixture = fixture();
    let signer = WebhookSigner::new(secret("current-key"));
    let (first, body, _) =
        send(&fixture.app, signed_request("/hooks/github", &signer, "msg_1", &issue(42))).await;
    assert_eq!(first, StatusCode::ACCEPTED);
    let run_id = body["runId"].clone();

    // A retry re-signs the same webhook-id at a later timestamp.
    let (again, body, _) =
        send(&fixture.app, signed_request("/hooks/github", &signer, "msg_1", &issue(42))).await;
    assert_eq!(again, StatusCode::OK);
    assert_eq!((body["runId"].clone(), body["duplicate"].clone()), (run_id.clone(), json!(true)));

    assert_eq!(run_queued(&fixture).await, 1);
    // A replay after the run finished still answers with that run and starts nothing.
    let (late, body, _) =
        send(&fixture.app, signed_request("/hooks/github", &signer, "msg_1", &issue(42))).await;
    assert_eq!(
        (late, body["runId"].clone(), body["state"].clone()),
        (StatusCode::OK, run_id, json!("completed"))
    );
    assert_eq!(run_queued(&fixture).await, 0);
    assert_eq!(fixture.store.list(RunFilter::default()).await.unwrap().len(), 1);
    assert_eq!(fixture.agent.runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn invalid_expired_and_unsigned_deliveries_are_unauthorized() {
    let fixture = fixture();
    let wrong_key = WebhookSigner::new(secret("attacker-key"));
    let (status, body, _) =
        send(&fixture.app, signed_request("/hooks/github", &wrong_key, "msg_1", &issue(1))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body, json!({"error": "unauthorized"}), "the reason is not disclosed");

    let ten_minutes_ago = ManualClock::new(chrono::Utc::now() - chrono::Duration::minutes(10));
    let stale = WebhookSigner::new(secret("current-key")).with_clock(Arc::new(ten_minutes_ago));
    let (status, _, _) =
        send(&fixture.app, signed_request("/hooks/github", &stale, "msg_2", &issue(2))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "an expired timestamp is refused");

    let unsigned = Request::post("/hooks/github")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&issue(3)).unwrap()))
        .unwrap();
    assert_eq!(send(&fixture.app, unsigned).await.0, StatusCode::UNAUTHORIZED);

    // A signature over a different body.
    let signer = WebhookSigner::new(secret("current-key"));
    let mut tampered = signed_request("/hooks/github", &signer, "msg_4", &issue(4));
    *tampered.body_mut() = Body::from(serde_json::to_vec(&issue(5)).unwrap());
    assert_eq!(send(&fixture.app, tampered).await.0, StatusCode::UNAUTHORIZED);

    assert!(fixture.store.list(RunFilter::default()).await.unwrap().is_empty());
}

#[tokio::test]
async fn either_key_is_accepted_during_a_rotation() {
    let fixture = fixture();
    for (key, id) in [("current-key", "msg_a"), ("next-key", "msg_b")] {
        let signer = WebhookSigner::new(secret(key));
        let (status, _, _) =
            send(&fixture.app, signed_request("/hooks/github", &signer, id, &issue(1))).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{key}");
    }
    let both = WebhookSigner::new(secret("retired-key")).with_secret(secret("next-key"));
    let (status, _, _) =
        send(&fixture.app, signed_request("/hooks/github", &both, "msg_c", &issue(1))).await;
    assert_eq!(status, StatusCode::ACCEPTED, "one matching signature among several is enough");
}

#[tokio::test]
async fn a_frozen_kill_switch_refuses_deliveries_with_503() {
    let fixture = fixture();
    fixture.governance.freeze("incident 12");
    let signer = WebhookSigner::new(secret("current-key"));
    let (status, body, retry_after) =
        send(&fixture.app, signed_request("/hooks/github", &signer, "msg_1", &issue(1))).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], "governance.frozen");
    assert_eq!(retry_after.as_deref(), Some("7"));
    assert!(fixture.store.list(RunFilter::default()).await.unwrap().is_empty());

    // The sender's retry after the unfreeze is accepted.
    fixture.governance.unfreeze();
    let (status, _, _) =
        send(&fixture.app, signed_request("/hooks/github", &signer, "msg_1", &issue(1))).await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn max_pending_throttles_new_deliveries_but_answers_redeliveries() {
    let fixture = fixture_with(|hook| hook.with_max_pending(1));
    let signer = WebhookSigner::new(secret("current-key"));
    let (status, body, _) =
        send(&fixture.app, signed_request("/hooks/github", &signer, "msg_1", &issue(1))).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, _, retry_after) =
        send(&fixture.app, signed_request("/hooks/github", &signer, "msg_2", &issue(2))).await;
    assert_eq!((status, retry_after.as_deref()), (StatusCode::TOO_MANY_REQUESTS, Some("7")));
    let (status, again, _) =
        send(&fixture.app, signed_request("/hooks/github", &signer, "msg_1", &issue(1))).await;
    assert_eq!((status, again["runId"].clone()), (StatusCode::OK, body["runId"].clone()));

    run_queued(&fixture).await;
    let (status, _, _) =
        send(&fixture.app, signed_request("/hooks/github", &signer, "msg_2", &issue(2))).await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn the_concurrency_key_and_on_conflict_are_configurable_per_hook() {
    let fixture = fixture_with(|hook| {
        hook.with_concurrency_key("repo:{{/payload/repository/id}}", OnConflict::Reject)
    });
    let signer = WebhookSigner::new(secret("current-key"));
    let (status, first, _) =
        send(&fixture.app, signed_request("/hooks/github", &signer, "msg_1", &issue(1))).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, body, _) =
        send(&fixture.app, signed_request("/hooks/github", &signer, "msg_2", &issue(2))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["blockingRunId"], first["runId"]);
    let record = fixture.store.get(first["runId"].as_str().unwrap()).await.unwrap().unwrap();
    assert_eq!(record.concurrency_key.as_deref(), Some("repo:99"));
}

#[tokio::test]
async fn unknown_hooks_bad_bodies_and_empty_keys_are_refused() {
    let fixture = fixture_with(|hook| hook.with_session_id("thread:{{/payload/thread}}"));
    let signer = WebhookSigner::new(secret("current-key"));
    let (status, _, _) =
        send(&fixture.app, signed_request("/hooks/nope", &signer, "msg_1", &issue(1))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Signed, but the session template renders empty.
    let (status, body, _) =
        send(&fixture.app, signed_request("/hooks/github", &signer, "msg_2", &issue(1))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    let raw = b"not json".to_vec();
    let signed = signer.sign_now("msg_3", &raw);
    let mut request = Request::post("/hooks/github");
    for (name, value) in signed.header_pairs() {
        request = request.header(name, value);
    }
    let (status, _, _) = send(&fixture.app, request.body(Body::from(raw)).unwrap()).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(fixture.store.list(RunFilter::default()).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_custom_mapper_keeps_the_webhook_id_as_idempotency_key() {
    let fixture = fixture_with(|hook| {
        hook.with_mapper(|delivery| {
            Ok(NewRun::new("default", "triage", "bot", "triage_agent", json!({"text": "custom"}))
                .with_idempotency_key("ignored")
                .with_session_id(format!("issue-{}", delivery.payload["issue"]["number"])))
        })
    });
    let signer = WebhookSigner::new(secret("current-key"));
    let (status, body, _) =
        send(&fixture.app, signed_request("/hooks/github", &signer, "msg_9", &issue(5))).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let record = fixture.store.get(body["runId"].as_str().unwrap()).await.unwrap().unwrap();
    assert_eq!(
        (record.idempotency_key.as_deref(), record.session_id.as_deref()),
        (Some("hook:github:msg_9"), Some("issue-5"))
    );
}
