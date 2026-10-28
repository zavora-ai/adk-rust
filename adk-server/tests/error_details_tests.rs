//! Error responses under `SecurityConfig::expose_error_details`: a provider's or a store's
//! message stays out of the run endpoints' Problem JSON, their SSE error events, and the
//! governance `503`, and is logged with the request's `x-request-id`; a message the server
//! wrote about the request is kept.

use adk_agent::LlmAgentBuilder;
use adk_core::governance::GovernanceMirrorConfig;
use adk_core::governance_state::{GovernanceScope, GovernanceState, GovernanceStateStore};
use adk_core::principal::SubjectRef;
use adk_core::{
    AdkError, ErrorCategory, ErrorComponent, GovernanceControl, Result as AdkResult,
    SingleAgentLoader,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::{InMemorySessionService, SessionService};
use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

const APP: &str = "assistant";
const PROVIDER_TEXT: &str = "invalid x-api-key for workspace wrkspc_7731 (org acme-prod)";
const DRIVER_TEXT: &str = "error returned from database: relation \"adk_governance_state\" \
                           does not exist (db-primary.internal:5432)";

/// The error `adk-model` builds from an Anthropic `401`.
fn provider_error() -> AdkError {
    AdkError::new(
        ErrorComponent::Model,
        ErrorCategory::Unauthorized,
        "model.anthropic.api_error",
        format!("Anthropic API error (401): {PROVIDER_TEXT} [type=authentication_error]"),
    )
    .with_upstream_status(401)
    .with_provider("anthropic")
    .with_request_id("req_011CXk")
}

/// A server whose agent's model fails every call with [`provider_error`].
fn failing_model_app(expose_details: bool) -> Router {
    let model = Arc::new(ScriptedLlm::new(
        "claude-sonnet-5-5",
        [ScriptedResponse::error(provider_error())],
    ));
    let agent = LlmAgentBuilder::new(APP).model(model).build().unwrap();
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let config = ServerConfig::new(Arc::new(SingleAgentLoader::new(Arc::new(agent))), sessions)
        .with_error_details(expose_details);
    ServerBuilder::new(config).build()
}

/// The tracing output of the current thread, which serves the requests a test sends.
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl Logs {
    fn capture() -> (Self, tracing::subscriber::DefaultGuard) {
        let logs = Self::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::INFO)
            .finish();
        (logs, tracing::subscriber::set_default(subscriber))
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }

    /// The log lines written inside the request `request_id`'s span.
    fn of_request(&self, request_id: &str) -> Vec<String> {
        let marker = format!("request.id={request_id}");
        self.text().lines().filter(|line| line.contains(&marker)).map(str::to_string).collect()
    }
}

impl std::io::Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logs {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Posts `body` to `uri` and returns the status, the `x-request-id`, and the body text.
async fn post(app: &Router, uri: &str, body: Value) -> (StatusCode, String, String) {
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, request_id, String::from_utf8(bytes.to_vec()).unwrap())
}

fn sse_events(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str(data).ok())
        .collect()
}

fn run_body() -> Value {
    json!({
        "appName": APP,
        "userId": "alice",
        "sessionId": "s-1",
        "newMessage": { "role": "user", "parts": [{ "text": "Hello" }] },
    })
}

fn ag_ui_body() -> Value {
    json!({
        "appName": APP,
        "userId": "alice",
        "sessionId": "thread-1",
        "uiProtocol": "ag_ui",
        "uiTransport": "protocol_native",
        "input": {
            "threadId": "thread-1",
            "runId": "run-1",
            "messages": [{ "id": "m1", "role": "user", "content": "Hello" }],
        },
    })
}

#[tokio::test]
async fn a_provider_message_stays_out_of_the_problem_json_and_is_logged_with_the_request_id() {
    let (logs, _guard) = Logs::capture();
    let app = failing_model_app(false);

    let (status, request_id, body) = post(&app, "/api/run", run_body()).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap(),
        json!({
            "error": {
                "code": "model.anthropic.api_error",
                "message": "Authentication failed",
                "component": "model",
                "category": "unauthorized",
                "requestId": "req_011CXk",
                "retryAfter": null,
                "upstreamStatusCode": 401,
            }
        })
    );
    let logged = logs.of_request(&request_id);
    assert!(
        logged.iter().any(|line| line.contains("error details withheld from the response")
            && line.contains(PROVIDER_TEXT)),
        "the full error is logged with request id {request_id}: {logged:#?}"
    );
}

#[tokio::test]
async fn exposed_details_keep_the_provider_message_in_the_problem_json() {
    let app = failing_model_app(true);

    let (status, _, body) = post(&app, "/api/run", run_body()).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["error"]["message"], provider_error().message);
}

#[tokio::test]
async fn a_provider_message_stays_out_of_the_sse_error_event() {
    for (expose_details, expected) in [
        (false, "model.unauthorized: Authentication failed".to_string()),
        (true, provider_error().to_string()),
    ] {
        let app = failing_model_app(expose_details);

        let (status, _, text) = post(&app, "/api/run_sse", run_body()).await;

        assert_eq!(status, StatusCode::OK);
        let last = sse_events(&text).pop().expect("the stream ends with an error event");
        assert_eq!(
            (last["error_code"].clone(), last["error_message"].clone()),
            (json!("model.anthropic.api_error"), json!(expected)),
            "expose_error_details = {expose_details}"
        );
    }
}

#[tokio::test]
async fn a_provider_message_stays_out_of_the_ag_ui_run_error() {
    for (expose_details, expected) in [
        (false, "model.unauthorized: Authentication failed".to_string()),
        (true, provider_error().to_string()),
    ] {
        let app = failing_model_app(expose_details);

        let (status, _, text) = post(&app, "/api/run_sse", ag_ui_body()).await;

        assert_eq!(status, StatusCode::OK);
        let events = sse_events(&text);
        let run_error = events
            .iter()
            .find(|event| event["type"] == "RUN_ERROR")
            .expect("the run ends with RUN_ERROR");
        assert_eq!(
            run_error,
            &json!({
                "type": "RUN_ERROR",
                "threadId": "thread-1",
                "runId": "run-1",
                "message": expected,
                "code": "model.anthropic.api_error",
            }),
            "expose_error_details = {expose_details}"
        );
    }
}

#[tokio::test]
async fn a_message_the_server_wrote_about_the_request_is_kept() {
    let app = failing_model_app(false);

    let (status, _, body) =
        post(&app, &format!("/api/run/{APP}/alice/s-missing"), json!({ "new_message": "Hi" }))
            .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        body["error"]["message"],
        "session 's-missing' was not found for app 'assistant' and user 'alice'"
    );
}

/// A governance store whose database is down.
struct UnreachableGovernanceStore;

impl UnreachableGovernanceStore {
    fn error() -> AdkError {
        AdkError::new(
            ErrorComponent::Session,
            ErrorCategory::Unavailable,
            "session.governance_store.storage",
            format!("governance read failed: {DRIVER_TEXT}"),
        )
    }
}

#[async_trait]
impl GovernanceStateStore for UnreachableGovernanceStore {
    async fn get(&self, _scope: &GovernanceScope) -> AdkResult<Option<GovernanceState>> {
        Err(Self::error())
    }

    async fn set(
        &self,
        _scope: GovernanceScope,
        _expected_epoch: u64,
        _frozen: bool,
        _reason: Option<String>,
        _issued_by: Option<SubjectRef>,
    ) -> AdkResult<GovernanceState> {
        Err(Self::error())
    }

    async fn list(&self) -> AdkResult<Vec<GovernanceState>> {
        Err(Self::error())
    }
}

#[tokio::test]
async fn a_store_message_stays_out_of_the_governance_503() {
    let generic =
        "session.unavailable: A service the server depends on is unavailable; retry later";
    for (expose_details, expected) in
        [(false, generic.to_string()), (true, UnreachableGovernanceStore::error().to_string())]
    {
        let control = GovernanceControl::mirrored(
            Arc::new(UnreachableGovernanceStore),
            GovernanceMirrorConfig::default(),
        );
        let agent = LlmAgentBuilder::new(APP)
            .model(Arc::new(ScriptedLlm::new("m", Vec::<ScriptedResponse>::new())))
            .build()
            .unwrap();
        let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
        let config = ServerConfig::new(Arc::new(SingleAgentLoader::new(Arc::new(agent))), sessions)
            .with_governance(control)
            .with_error_details(expose_details);
        let app = ServerBuilder::new(config).enable_governance_endpoints().build();

        for (uri, body) in [
            ("/api/admin/freeze", json!({ "reason": "incident 7" })),
            ("/api/admin/unfreeze", json!({})),
        ] {
            let (status, _, text) = post(&app, uri, body).await;

            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{uri}");
            let status: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(status["error"], expected, "{uri}, expose_error_details = {expose_details}");
        }
    }
}
