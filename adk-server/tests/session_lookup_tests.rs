//! The REST and legacy A2A run endpoints create a session only when it does not exist: a
//! transient lookup failure fails the request and leaves the stored session as it was, two
//! first requests racing to create a session both run, and session errors map to their HTTP
//! status.

use adk_core::identity::AdkIdentity;
use adk_core::{AdkError, Content, ErrorComponent, Event};
use adk_server::{ServerConfig, create_app_with_a2a};
use adk_session::{
    AppendEventRequest, CreateRequest, DeleteRequest, GetRequest, InMemorySessionService,
    ListRequest, Session, SessionService,
};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tower::ServiceExt;

const APP: &str = "echo";

struct Loader;

#[async_trait]
impl adk_core::AgentLoader for Loader {
    async fn load_agent(&self, _app: &str) -> adk_core::Result<Arc<dyn adk_core::Agent>> {
        Ok(Arc::new(Echo))
    }

    fn list_agents(&self) -> Vec<String> {
        vec![APP.to_string()]
    }

    fn root_agent(&self) -> Arc<dyn adk_core::Agent> {
        Arc::new(Echo)
    }
}

/// Answers with the number of history items it sees.
struct Echo;

#[async_trait]
impl adk_core::Agent for Echo {
    fn name(&self) -> &str {
        APP
    }

    fn description(&self) -> &str {
        "reports the history it sees"
    }

    fn sub_agents(&self) -> &[Arc<dyn adk_core::Agent>] {
        &[]
    }

    async fn run(
        &self,
        ctx: Arc<dyn adk_core::InvocationContext>,
    ) -> adk_core::Result<adk_core::EventStream> {
        let seen = ctx.session().conversation_history().len();
        Ok(Box::pin(async_stream::stream! {
            let mut event = Event::new("echo");
            event.author = APP.to_string();
            event.set_content(Content::new("model").with_text(format!("I see {seen} items")));
            yield Ok(event);
        }))
    }
}

/// Fails the next `get` with a transient error when armed, and can hold `create` until two
/// calls wait, so first requests race.
struct Faults {
    inner: InMemorySessionService,
    fail_next_get: AtomicBool,
    create_gate: std::sync::Mutex<Option<Arc<tokio::sync::Barrier>>>,
}

#[async_trait]
impl SessionService for Faults {
    async fn create(&self, req: CreateRequest) -> adk_core::Result<Box<dyn Session>> {
        let gate = self.create_gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            gate.wait().await;
        }
        self.inner.create(req).await
    }

    async fn get(&self, req: GetRequest) -> adk_core::Result<Box<dyn Session>> {
        if self.fail_next_get.swap(false, Ordering::SeqCst) {
            return Err(AdkError::unavailable(
                ErrorComponent::Session,
                "session.backend.timeout",
                "the session backend timed out",
            ));
        }
        self.inner.get(req).await
    }

    async fn list(&self, req: ListRequest) -> adk_core::Result<Vec<Box<dyn Session>>> {
        self.inner.list(req).await
    }

    async fn delete(&self, req: DeleteRequest) -> adk_core::Result<()> {
        self.inner.delete(req).await
    }

    async fn append_event(&self, session_id: &str, event: Event) -> adk_core::Result<()> {
        self.inner.append_event(session_id, event).await
    }

    async fn append_event_for_identity(&self, req: AppendEventRequest) -> adk_core::Result<()> {
        self.inner.append_event_for_identity(req).await
    }
}

fn faults() -> Arc<Faults> {
    Arc::new(Faults {
        inner: InMemorySessionService::new(),
        fail_next_get: AtomicBool::new(false),
        create_gate: std::sync::Mutex::new(None),
    })
}

fn app(sessions: Arc<Faults>) -> axum::Router {
    create_app_with_a2a(
        ServerConfig::new(Arc::new(Loader), sessions as Arc<dyn SessionService>),
        Some("http://localhost:8080"),
    )
}

/// Seeds `(app, user, session)` with `plan = gold` and three events.
async fn seed(sessions: &Faults, app: &str, user: &str, session: &str) {
    sessions
        .create(CreateRequest {
            app_name: app.to_string(),
            user_id: user.to_string(),
            session_id: Some(session.to_string()),
            state: HashMap::from([("plan".to_string(), json!("gold"))]),
        })
        .await
        .unwrap();
    let identity = AdkIdentity::new(
        app.try_into().unwrap(),
        user.try_into().unwrap(),
        session.try_into().unwrap(),
    );
    for n in 0..3 {
        let mut event = Event::new("earlier");
        event.author = "user".to_string();
        event.set_content(Content::new("user").with_text(format!("earlier turn {n}")));
        sessions
            .append_event_for_identity(AppendEventRequest { identity: identity.clone(), event })
            .await
            .unwrap();
    }
}

async fn stored(sessions: &Faults, app: &str, user: &str, session: &str) -> (usize, Option<Value>) {
    let session = sessions
        .inner
        .get(GetRequest {
            app_name: app.to_string(),
            user_id: user.to_string(),
            session_id: session.to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap();
    (session.events().len(), session.state().get("plan"))
}

async fn post(app: &axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::post(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = http_body_util::BodyExt::collect(response.into_body()).await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn run_body(user: &str, session: &str) -> Value {
    json!({
        "appName": APP,
        "userId": user,
        "sessionId": session,
        "newMessage": {"role": "user", "parts": [{"text": "continue"}]},
    })
}

#[tokio::test]
async fn transient_lookup_failure_fails_the_run_and_keeps_the_session() {
    for uri in ["/api/run", "/api/run_sse"] {
        let sessions = faults();
        let app = app(sessions.clone());
        seed(&sessions, APP, "u1", "s1").await;

        sessions.fail_next_get.store(true, Ordering::SeqCst);
        let (status, _) = post(&app, uri, run_body("u1", "s1")).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{uri}");
        assert_eq!(stored(&sessions, APP, "u1", "s1").await, (3, Some(json!("gold"))), "{uri}");

        let (status, _) = post(&app, uri, run_body("u1", "s1")).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_eq!(stored(&sessions, APP, "u1", "s1").await, (5, Some(json!("gold"))), "{uri}");
    }
}

#[tokio::test]
async fn path_run_reports_a_transient_lookup_failure_as_unavailable() {
    let sessions = faults();
    let app = app(sessions.clone());
    seed(&sessions, APP, "u1", "s1").await;

    sessions.fail_next_get.store(true, Ordering::SeqCst);
    let (status, _) =
        post(&app, &format!("/api/run/{APP}/u1/s1"), json!({"new_message": "continue"})).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

    let (status, _) =
        post(&app, &format!("/api/run/{APP}/u1/missing"), json!({"new_message": "hi"})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn racing_first_requests_both_run_on_one_session() {
    let sessions = faults();
    let app = app(sessions.clone());
    *sessions.create_gate.lock().unwrap() = Some(Arc::new(tokio::sync::Barrier::new(2)));

    let (first, second) = tokio::join!(
        post(&app, "/api/run", run_body("u1", "new")),
        post(&app, "/api/run", run_body("u1", "new"))
    );
    assert_eq!((first.0, second.0), (StatusCode::OK, StatusCode::OK));

    let session = sessions
        .inner
        .get(GetRequest {
            app_name: APP.to_string(),
            user_id: "u1".to_string(),
            session_id: "new".to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .unwrap();
    let user_turns = session.events().all().iter().filter(|e| e.author == "user").count();
    assert_eq!(user_turns, 2);
}

#[tokio::test]
async fn legacy_a2a_keeps_the_session_when_its_lookup_fails() {
    let sessions = faults();
    let app = app(sessions.clone());
    let user = "A2A_USER_ctx1";
    seed(&sessions, APP, user, "ctx1").await;
    let send = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "message/send",
        "params": {"message": {
            "role": "user",
            "parts": [{"text": "continue"}],
            "messageId": "m1",
            "contextId": "ctx1",
        }},
    });

    sessions.fail_next_get.store(true, Ordering::SeqCst);
    let (_, response) = post(&app, "/a2a", send.clone()).await;
    assert!(response.get("error").is_some(), "{response}");
    assert_eq!(stored(&sessions, APP, user, "ctx1").await, (3, Some(json!("gold"))));

    let mut retry = send;
    retry["params"]["message"]["messageId"] = json!("m2");
    let (_, response) = post(&app, "/a2a", retry).await;
    assert!(response.get("result").is_some(), "{response}");
    assert_eq!(stored(&sessions, APP, user, "ctx1").await.0, 5);
}

#[tokio::test]
async fn session_endpoints_map_errors_to_their_status() {
    let sessions = faults();
    let app = app(sessions.clone());
    seed(&sessions, APP, "u1", "s1").await;

    let (status, _) = post(&app, &format!("/api/apps/{APP}/users/u1/sessions/s1"), json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) =
        post(&app, "/api/sessions", json!({"appName": APP, "userId": "u1", "sessionId": "s1"}))
            .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(stored(&sessions, APP, "u1", "s1").await, (3, Some(json!("gold"))));

    let get = |uri: String| {
        let app = app.clone();
        async move {
            app.oneshot(Request::get(uri).body(Body::empty()).unwrap()).await.unwrap().status()
        }
    };
    assert_eq!(
        get(format!("/api/apps/{APP}/users/u1/sessions/missing")).await,
        StatusCode::NOT_FOUND
    );
    sessions.fail_next_get.store(true, Ordering::SeqCst);
    assert_eq!(
        get(format!("/api/apps/{APP}/users/u1/sessions/s1")).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
}
