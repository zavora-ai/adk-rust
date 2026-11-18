//! The session history routes: read with a version, fork, rewind by event, steps, or
//! invocation, and edit, behind the session routes' authentication, refused while a turn runs on
//! the session, and recorded in the governance audit trail.

use adk_core::audit_log::{AuditRecordKind, AuditRecordOutcome, AuditTrail, InMemoryAuditRecorder};
use adk_core::identity::AdkIdentity;
use adk_core::{Content, Event};
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::{ServerConfig, create_app};
use adk_session::{AppendEventRequest, CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tower::ServiceExt;

const APP: &str = "history";

struct Loader;

#[async_trait]
impl adk_core::AgentLoader for Loader {
    async fn load_agent(&self, _app: &str) -> adk_core::Result<Arc<dyn adk_core::Agent>> {
        Err(adk_core::AdkError::agent("no agent runs in these tests"))
    }

    fn list_agents(&self) -> Vec<String> {
        vec![APP.to_string()]
    }

    fn root_agent(&self) -> Arc<dyn adk_core::Agent> {
        unimplemented!("no agent runs in these tests")
    }
}

/// `Bearer <user>` authenticates `<user>`.
struct UserExtractor;

#[async_trait]
impl RequestContextExtractor for UserExtractor {
    async fn extract(
        &self,
        parts: &axum::http::request::Parts,
    ) -> Result<adk_core::RequestContext, RequestContextError> {
        let user = parts
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .ok_or(RequestContextError::MissingAuth)?;
        Ok(adk_core::RequestContext {
            user_id: user.to_string(),
            scopes: vec![],
            metadata: Default::default(),
        })
    }
}

struct Fixture {
    app: Router,
    sessions: Arc<InMemorySessionService>,
    config: ServerConfig,
    recorder: Arc<InMemoryAuditRecorder>,
    /// Event IDs of alice's session, in order; events 0 and 1 are invocation `inv-0`.
    events: Vec<String>,
}

async fn fixture() -> Fixture {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: APP.into(),
            user_id: "alice".into(),
            session_id: Some("s1".into()),
            state: HashMap::from([("plan".to_string(), json!("gold"))]),
        })
        .await
        .unwrap();
    let identity = AdkIdentity::new(
        APP.try_into().unwrap(),
        "alice".try_into().unwrap(),
        "s1".try_into().unwrap(),
    );
    let mut events = Vec::new();
    for n in 0..4 {
        let mut event = Event::new(format!("inv-{}", n / 2));
        event.author = if n % 2 == 0 { "user" } else { "agent" }.to_string();
        event.set_content(
            Content::new(if n % 2 == 0 { "user" } else { "model" }).with_text(format!("turn {n}")),
        );
        event.actions.state_delta.insert("step".into(), json!(n));
        events.push(event.id.clone());
        sessions
            .append_event_for_identity(AppendEventRequest { identity: identity.clone(), event })
            .await
            .unwrap();
    }
    let recorder = Arc::new(InMemoryAuditRecorder::new());
    let governance = adk_core::GovernanceControl::new();
    governance.attach_audit(AuditTrail::new(recorder.clone()));
    let config = ServerConfig::new(Arc::new(Loader), sessions.clone() as Arc<dyn SessionService>)
        .with_governance(governance)
        .with_request_context(Arc::new(UserExtractor));
    Fixture { app: create_app(config.clone()), sessions, config, recorder, events }
}

const BASE: &str = "/api/apps/history/users/alice/sessions/s1";

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    user: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {user}"))
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = http_body_util::BodyExt::collect(response.into_body()).await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn texts(session: &Value) -> Vec<String> {
    session["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["content"]["parts"][0]["text"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
async fn history_reads_with_a_version_and_only_for_its_user() {
    let fixture = fixture().await;
    let (status, body) = call(&fixture.app, "GET", &format!("{BASE}/history"), "alice", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(texts(&body["session"]).len(), 4);
    assert!(body["version"].as_str().unwrap().starts_with("fnv1a128:"));
    assert!(body.get("forkedFrom").is_none());

    for (method, path, body) in [
        ("GET", "history", None),
        ("POST", "fork", Some(json!({}))),
        ("POST", "rewind", Some(json!({"steps": 1}))),
        ("POST", "history/edit", Some(json!({"edits": [{"op": "remove", "eventId": "e"}]}))),
    ] {
        let (status, body) =
            call(&fixture.app, method, &format!("{BASE}/{path}"), "mallory", body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}: {body}");
    }
    let (status, _) = call(
        &fixture.app,
        "GET",
        "/api/apps/history/users/alice/sessions/nope/history",
        "alice",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn fork_copies_the_events_up_to_the_fork_point_and_records_it() {
    let fixture = fixture().await;
    let (status, body) = call(
        &fixture.app,
        "POST",
        &format!("{BASE}/fork"),
        "alice",
        Some(json!({"eventId": fixture.events[1], "sessionId": "s1-b"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["session"]["id"], "s1-b");
    assert_eq!(texts(&body["session"]), ["turn 0", "turn 1"]);
    assert_eq!(body["session"]["state"]["step"], 1);
    assert_eq!(body["change"]["operation"], "fork");
    assert_eq!(body["change"]["forkOrigin"]["sessionId"], "s1");

    let (_, fork) = call(
        &fixture.app,
        "GET",
        "/api/apps/history/users/alice/sessions/s1-b/history",
        "alice",
        None,
    )
    .await;
    assert_eq!(fork["forkedFrom"]["eventId"], fixture.events[1].as_str());
    let (_, source) = call(&fixture.app, "GET", &format!("{BASE}/history"), "alice", None).await;
    assert_eq!(texts(&source["session"]).len(), 4, "the source is untouched");

    let (status, body) = call(
        &fixture.app,
        "POST",
        &format!("{BASE}/fork"),
        "alice",
        Some(json!({"sessionId": "s1-b"})),
    )
    .await;
    assert_eq!(
        (status, body["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("session.already_exists"))
    );

    let records = fixture.recorder.records();
    let fork_records: Vec<_> = records
        .iter()
        .filter(|record| {
            record.kind == AuditRecordKind::SessionHistory && record.action == "session.fork"
        })
        .collect();
    let outcomes: Vec<_> = fork_records.iter().map(|record| record.outcome).collect();
    assert_eq!(
        outcomes,
        [None, Some(AuditRecordOutcome::Succeeded), None, Some(AuditRecordOutcome::Failed)],
        "a decision and an outcome for each fork"
    );
    assert_eq!(fork_records[1].ext["fork"], "s1-b");
    assert_eq!(fork_records[3].ext["error"], "session.already_exists");
}

#[tokio::test]
async fn rewind_by_event_steps_or_invocation_with_a_version_guard() {
    let fixture = fixture().await;
    let (_, read) = call(&fixture.app, "GET", &format!("{BASE}/history"), "alice", None).await;
    let version = read["version"].as_str().unwrap().to_string();

    let (status, body) = call(
        &fixture.app,
        "POST",
        &format!("{BASE}/rewind"),
        "alice",
        Some(json!({"steps": 1, "eventId": "x"})),
    )
    .await;
    assert_eq!(
        (status, body["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("session.rewind.target_required"))
    );

    let (status, body) = call(
        &fixture.app,
        "POST",
        &format!("{BASE}/rewind"),
        "alice",
        Some(json!({"beforeInvocationId": "inv-1", "expectedVersion": version})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(texts(&body["session"]), ["turn 0", "turn 1"]);
    assert_eq!(body["session"]["state"]["step"], 1);
    assert_eq!(body["change"]["removed"], json!([fixture.events[2], fixture.events[3]]));

    let (status, body) = call(
        &fixture.app,
        "POST",
        &format!("{BASE}/rewind"),
        "alice",
        Some(json!({"steps": 1, "expectedVersion": version})),
    )
    .await;
    assert_eq!(
        (status, body["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("session.history.version_mismatch"))
    );

    let (status, body) = call(
        &fixture.app,
        "POST",
        &format!("{BASE}/rewind"),
        "alice",
        Some(json!({"eventId": fixture.events[0]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(texts(&body["session"]), ["turn 0"]);
    let (status, body) =
        call(&fixture.app, "POST", &format!("{BASE}/rewind"), "alice", Some(json!({"steps": 5})))
            .await;
    assert_eq!(
        (status, body["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("session.rewind.too_many_steps"))
    );
}

#[tokio::test]
async fn edit_replaces_and_removes_events() {
    let fixture = fixture().await;
    let (_, read) = call(&fixture.app, "GET", &format!("{BASE}/history"), "alice", None).await;
    let mut replacement = read["session"]["events"][0].clone();
    replacement["content"]["parts"][0]["text"] = json!("[redacted]");
    let (status, body) = call(
        &fixture.app,
        "POST",
        &format!("{BASE}/history/edit"),
        "alice",
        Some(json!({
            "edits": [
                {"op": "replace", "event": replacement},
                {"op": "remove", "eventId": fixture.events[3]},
            ],
            "expectedVersion": read["version"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(texts(&body["session"]), ["[redacted]", "turn 1", "turn 2"]);
    assert_eq!(body["change"]["replaced"], json!([fixture.events[0]]));
    assert_eq!(body["change"]["removed"], json!([fixture.events[3]]));

    let (status, body) = call(
        &fixture.app,
        "POST",
        &format!("{BASE}/history/edit"),
        "alice",
        Some(json!({"edits": []})),
    )
    .await;
    assert_eq!(
        (status, body["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("session.history.no_edits"))
    );

    let records = format!("{:?}", fixture.recorder.records());
    assert!(!records.contains("turn 0"), "audit records carry IDs and digests, not content");
}

#[tokio::test]
async fn a_turn_running_on_the_session_refuses_every_history_route() {
    let fixture = fixture().await;
    let identity = AdkIdentity::new(
        APP.try_into().unwrap(),
        "alice".try_into().unwrap(),
        "s1".try_into().unwrap(),
    );
    let running = fixture.config.session_activity.enter(identity);
    for (path, body) in [
        ("fork", json!({})),
        ("rewind", json!({"steps": 1})),
        ("history/edit", json!({"edits": [{"op": "remove", "eventId": fixture.events[3]}]})),
    ] {
        let (status, body) =
            call(&fixture.app, "POST", &format!("{BASE}/{path}"), "alice", Some(body)).await;
        assert_eq!(
            (status, body["error"]["code"].as_str()),
            (StatusCode::CONFLICT, Some("session.history.run_active")),
            "{path}"
        );
    }
    drop(running);
    let (status, _) =
        call(&fixture.app, "POST", &format!("{BASE}/rewind"), "alice", Some(json!({"steps": 1})))
            .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        fixture
            .sessions
            .history(&AdkIdentity::new(
                APP.try_into().unwrap(),
                "alice".try_into().unwrap(),
                "s1".try_into().unwrap()
            ))
            .await
            .unwrap()
            .events
            .len(),
        3
    );
}
