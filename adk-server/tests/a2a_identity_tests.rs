//! The A2A JSON-RPC routes bind sessions and tasks to the authenticated principal.
//!
//! The session user used to be `A2A_USER_{contextId}`, derived from a value the client
//! chooses, and `tasks/get` / `tasks/cancel` looked tasks up by ID alone. An authenticated
//! caller who learned another caller's `contextId` or task ID could read that caller's
//! conversation history, poll its task, or cancel it.

use adk_core::{Agent, EventStream, InvocationContext, Result as AdkResult};
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::{A2aTaskRetention, ServerBuilder, ServerConfig, create_app_with_a2a};
use adk_session::{GetRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Notify;
use tower::ServiceExt;

const APP: &str = "history_agent";

/// Replies with every user text it can see in its session, after `gate` opens.
struct HistoryAgent {
    gate: Option<Arc<Notify>>,
}

#[async_trait]
impl Agent for HistoryAgent {
    fn name(&self) -> &str {
        APP
    }

    fn description(&self) -> &str {
        "echoes the session history it can see"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
        let gate = self.gate.clone();
        let invocation_id = ctx.invocation_id().to_string();
        let seen: Vec<String> = ctx
            .session()
            .conversation_history()
            .iter()
            .filter(|content| content.role == "user")
            .flat_map(|content| content.parts.iter().filter_map(|part| part.text()))
            .map(str::to_string)
            .collect();
        Ok(Box::pin(async_stream::stream! {
            if let Some(gate) = gate {
                gate.notified().await;
            }
            let mut event = adk_core::Event::new(invocation_id);
            event.author = APP.to_string();
            event.llm_response.content =
                Some(adk_core::Content::new("model").with_text(seen.join("|")));
            yield Ok(event);
        }))
    }
}

/// Authenticates the user named in the `x-user` header.
struct HeaderExtractor;

#[async_trait]
impl RequestContextExtractor for HeaderExtractor {
    async fn extract(
        &self,
        parts: &axum::http::request::Parts,
    ) -> Result<adk_core::RequestContext, RequestContextError> {
        let user_id = parts
            .headers
            .get("x-user")
            .and_then(|value| value.to_str().ok())
            .ok_or(RequestContextError::MissingAuth)?;
        Ok(adk_core::RequestContext {
            user_id: user_id.to_string(),
            scopes: vec![],
            metadata: Default::default(),
        })
    }
}

fn config(gate: Option<Arc<Notify>>, sessions: Arc<InMemorySessionService>) -> ServerConfig {
    let agent = Arc::new(HistoryAgent { gate });
    ServerConfig::new(Arc::new(adk_core::SingleAgentLoader::new(agent)), sessions)
}

fn authenticated_app(gate: Option<Arc<Notify>>, sessions: Arc<InMemorySessionService>) -> Router {
    let config = config(gate, sessions).with_request_context(Arc::new(HeaderExtractor));
    ServerBuilder::new(config).with_a2a("http://localhost:8080").build()
}

async fn rpc(app: &Router, user: Option<&str>, method: &str, params: Value) -> Value {
    let mut request =
        Request::builder().method("POST").uri("/a2a").header("content-type", "application/json");
    if let Some(user) = user {
        request = request.header("x-user", user);
    }
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let response =
        app.clone().oneshot(request.body(Body::from(body.to_string())).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn send_params(text: &str, context_id: &str, task_id: &str) -> Value {
    json!({
        "message": {
            "role": "user",
            "messageId": format!("msg-{task_id}"),
            "contextId": context_id,
            "taskId": task_id,
            "parts": [{ "text": text }]
        }
    })
}

/// The agent's reply text from a completed `message/send` result.
fn reply_text(response: &Value) -> &str {
    response["result"]["artifacts"][0]["parts"][0]["text"].as_str().unwrap_or_default()
}

fn not_found(task_id: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "error": { "code": -32603, "message": format!("Task not found: {task_id}") }
    })
}

async fn session_exists(
    sessions: &InMemorySessionService,
    user_id: &str,
    session_id: &str,
) -> bool {
    sessions
        .get(GetRequest {
            app_name: APP.to_string(),
            user_id: user_id.to_string(),
            session_id: session_id.to_string(),
            num_recent_events: None,
            after: None,
        })
        .await
        .is_ok()
}

#[tokio::test]
async fn a_tenant_reusing_another_tenants_context_id_does_not_see_its_history() {
    let sessions = Arc::new(InMemorySessionService::new());
    let app = authenticated_app(None, sessions.clone());

    let bob =
        rpc(&app, Some("bob"), "message/send", send_params("bob-secret", "shared", "b1")).await;
    assert_eq!(reply_text(&bob), "bob-secret");

    let alice =
        rpc(&app, Some("alice"), "message/send", send_params("alice-hello", "shared", "a1")).await;
    assert_eq!(reply_text(&alice), "alice-hello", "alice must see only her own history");

    assert!(session_exists(&sessions, "bob", "shared").await);
    assert!(session_exists(&sessions, "alice", "shared").await);
    assert!(
        !session_exists(&sessions, "A2A_USER_shared", "shared").await,
        "an authenticated caller must not fall back to the context-derived user"
    );
}

#[tokio::test]
async fn a_tenant_cannot_get_another_tenants_task() {
    let sessions = Arc::new(InMemorySessionService::new());
    let app = authenticated_app(None, sessions);

    rpc(&app, Some("bob"), "message/send", send_params("bob-secret", "bob-ctx", "bob-task")).await;

    let as_alice = rpc(&app, Some("alice"), "tasks/get", json!({ "taskId": "bob-task" })).await;
    let missing = rpc(&app, Some("alice"), "tasks/get", json!({ "taskId": "no-such-task" })).await;
    assert_eq!(as_alice, not_found("bob-task"));
    assert_eq!(missing, not_found("no-such-task"), "both answers have the same shape");

    let as_bob = rpc(&app, Some("bob"), "tasks/get", json!({ "taskId": "bob-task" })).await;
    assert_eq!(as_bob["result"]["status"]["state"], "completed");
}

#[tokio::test]
async fn a_tenant_cannot_cancel_or_observe_another_tenants_running_task() {
    let sessions = Arc::new(InMemorySessionService::new());
    let gate = Arc::new(Notify::new());
    let app = authenticated_app(Some(gate.clone()), sessions);

    let bob_app = app.clone();
    let bob_send = tokio::spawn(async move {
        rpc(&bob_app, Some("bob"), "message/send", send_params("bob-secret", "bob-ctx", "bob-task"))
            .await
    });

    // Wait until bob's task is registered as running.
    let mut running = Value::Null;
    for _ in 0..200 {
        running = rpc(&app, Some("bob"), "tasks/get", json!({ "taskId": "bob-task" })).await;
        if running["result"]["status"]["state"] == "working" {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(running["result"]["status"]["state"], "working");

    let alice_get = rpc(&app, Some("alice"), "tasks/get", json!({ "taskId": "bob-task" })).await;
    assert_eq!(alice_get, not_found("bob-task"));
    let alice_cancel =
        rpc(&app, Some("alice"), "tasks/cancel", json!({ "taskId": "bob-task" })).await;
    assert_eq!(alice_cancel, not_found("bob-task"));

    gate.notify_one();
    let bob = bob_send.await.unwrap();
    assert_eq!(bob["result"]["status"]["state"], "completed", "alice's cancel must not land");

    let alice_cancel_finished =
        rpc(&app, Some("alice"), "tasks/cancel", json!({ "taskId": "bob-task" })).await;
    assert_eq!(alice_cancel_finished, not_found("bob-task"));
}

#[tokio::test]
async fn a_tenant_cannot_take_over_another_tenants_task_id() {
    let sessions = Arc::new(InMemorySessionService::new());
    let app = authenticated_app(None, sessions);

    rpc(&app, Some("bob"), "message/send", send_params("bob-secret", "bob-ctx", "bob-task")).await;
    let takeover =
        rpc(&app, Some("alice"), "message/send", send_params("overwrite", "a-ctx", "bob-task"))
            .await;

    assert_eq!(
        takeover,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": { "code": -32602, "message": "task id 'bob-task' is already in use" }
        })
    );
    let as_bob = rpc(&app, Some("bob"), "tasks/get", json!({ "taskId": "bob-task" })).await;
    assert_eq!(reply_text(&as_bob), "bob-secret", "bob's record is unchanged");
}

#[tokio::test]
async fn without_authentication_sessions_stay_scoped_to_the_context_id() {
    let sessions = Arc::new(InMemorySessionService::new());
    let app = create_app_with_a2a(config(None, sessions.clone()), Some("http://localhost:8080"));

    let first = rpc(&app, None, "message/send", send_params("first", "ctx-1", "t1")).await;
    assert_eq!(reply_text(&first), "first");
    let second = rpc(&app, None, "message/send", send_params("second", "ctx-1", "t2")).await;
    assert_eq!(reply_text(&second), "first|second", "the context keeps its history");

    assert!(session_exists(&sessions, "A2A_USER_ctx-1", "ctx-1").await);
    let fetched = rpc(&app, None, "tasks/get", json!({ "taskId": "t1" })).await;
    assert_eq!(fetched["result"]["status"]["state"], "completed");
}

#[tokio::test]
async fn the_builder_bounds_retained_task_records() {
    let sessions = Arc::new(InMemorySessionService::new());
    let app = ServerBuilder::new(config(None, sessions))
        .with_a2a("http://localhost:8080")
        .with_a2a_task_retention(A2aTaskRetention::default().with_max_finished(1))
        .build();

    rpc(&app, None, "message/send", send_params("one", "ctx", "t1")).await;
    rpc(&app, None, "message/send", send_params("two", "ctx", "t2")).await;

    assert_eq!(rpc(&app, None, "tasks/get", json!({ "taskId": "t1" })).await, not_found("t1"));
    let newest = rpc(&app, None, "tasks/get", json!({ "taskId": "t2" })).await;
    assert_eq!(newest["result"]["status"]["state"], "completed");
}
