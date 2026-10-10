//! The governance kill switch: admin endpoints, the runners the server builds, and scheduling.

#![cfg(feature = "background")]

use adk_core::{
    Agent, EventStream, GovernanceControl, InvocationContext, Result as AdkResult,
    SingleAgentLoader,
};
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::background::{BackgroundState, CronState};
use adk_server::{ServerBuilder, ServerConfig};
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tower::ServiceExt;

/// Counts how often it runs.
struct CountingAgent {
    runs: Arc<AtomicUsize>,
}

#[async_trait]
impl Agent for CountingAgent {
    fn name(&self) -> &str {
        "counting_agent"
    }
    fn description(&self) -> &str {
        "counts runs"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }
    async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok(Box::pin(futures::stream::empty()))
    }
}

/// Accepts only the bearer token `good`.
struct TokenExtractor;

#[async_trait]
impl RequestContextExtractor for TokenExtractor {
    async fn extract(
        &self,
        parts: &axum::http::request::Parts,
    ) -> Result<adk_core::RequestContext, RequestContextError> {
        match parts.headers.get("authorization").and_then(|value| value.to_str().ok()) {
            Some("Bearer good") => Ok(adk_core::RequestContext {
                user_id: "operator".to_string(),
                scopes: vec![],
                metadata: Default::default(),
            }),
            Some(_) => Err(RequestContextError::InvalidToken("unknown token".to_string())),
            None => Err(RequestContextError::MissingAuth),
        }
    }
}

struct Fixture {
    app: Router,
    control: GovernanceControl,
    runs: Arc<AtomicUsize>,
    cron: CronState,
}

async fn fixture() -> Fixture {
    let runs = Arc::new(AtomicUsize::new(0));
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "counting_agent".to_string(),
            user_id: "operator".to_string(),
            session_id: Some("s1".to_string()),
            state: HashMap::new(),
        })
        .await
        .unwrap();
    let control = GovernanceControl::new();
    let loader = Arc::new(SingleAgentLoader::new(Arc::new(CountingAgent { runs: runs.clone() })));
    let config = ServerConfig::new(loader, sessions as Arc<dyn SessionService>)
        .with_request_context(Arc::new(TokenExtractor))
        .with_governance(control.clone());
    let background = BackgroundState::new();
    let cron = CronState::new(background.clone());
    let app = ServerBuilder::new(config)
        .with_background_runs(background)
        .with_cron_jobs(cron.clone())
        .enable_governance_endpoints()
        .build();
    Fixture { app, control, runs, cron }
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut builder =
        Request::builder().method(method).uri(uri).header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let response =
        app.clone().oneshot(builder.body(Body::from(body.to_string())).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn the_admin_endpoints_require_authentication() {
    let fixture = fixture().await;
    let (status, _) =
        call(&fixture.app, "POST", "/api/admin/freeze", None, json!({ "reason": "x" })).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(!fixture.control.is_frozen());
}

#[tokio::test]
async fn freezing_stops_runs_and_pauses_scheduling_until_unfrozen() {
    let fixture = fixture().await;
    let run_body = json!({
        "appName": "counting_agent",
        "userId": "operator",
        "sessionId": "s1",
        "newMessage": { "role": "user", "parts": [{ "text": "go" }] }
    });

    let (status, body) = call(
        &fixture.app,
        "POST",
        "/api/admin/freeze",
        Some("good"),
        json!({ "reason": "incident 4012" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "frozen": true, "reason": "incident 4012" }));
    assert!(fixture.control.is_frozen());
    assert!(fixture.cron.cron_store.is_scheduling_paused());
    assert!(fixture.cron.background_state.runner.is_paused());

    let (status, _) = call(&fixture.app, "POST", "/api/run", Some("good"), run_body.clone()).await;
    assert_ne!(status, StatusCode::OK, "a frozen server must not run the agent");
    assert_eq!(fixture.runs.load(Ordering::SeqCst), 0);

    let submit = json!({ "workflowId": "report", "input": {} });
    let (status, _) = call(&fixture.app, "POST", "/api/runs", Some("good"), submit).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

    let (status, body) =
        call(&fixture.app, "POST", "/api/admin/unfreeze", Some("good"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "frozen": false }));
    assert!(!fixture.cron.cron_store.is_scheduling_paused());

    let (status, _) = call(&fixture.app, "POST", "/api/run", Some("good"), run_body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fixture.runs.load(Ordering::SeqCst), 1);
}
