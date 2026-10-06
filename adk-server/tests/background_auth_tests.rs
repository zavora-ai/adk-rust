//! Background run and cron routes mounted through `ServerBuilder` require authentication.
//!
//! `background_runs_router` and `cron_jobs_router` carry no auth of their own, and the
//! builder had no way to mount them, so they ended up merged at the root where anyone
//! who reached the port could submit, schedule, and cancel work.

#![cfg(feature = "background")]

use adk_core::{Agent, EventStream, InvocationContext, Result as AdkResult, SingleAgentLoader};
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::background::{BackgroundState, CronState};
use adk_server::{ServerBuilder, ServerConfig};
use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use std::sync::Arc;
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

fn app() -> Router {
    let loader = Arc::new(SingleAgentLoader::new(Arc::new(IdleAgent)));
    let config = ServerConfig::new(loader, Arc::new(adk_session::InMemorySessionService::new()))
        .with_request_context(Arc::new(TokenExtractor));
    let background = BackgroundState::new();
    ServerBuilder::new(config)
        .with_background_runs(background.clone())
        .with_cron_jobs(CronState::new(background))
        .build()
}

fn request(
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Option<serde_json::Value>,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

fn cron_body() -> serde_json::Value {
    json!({ "name": "nightly", "workflowId": "report", "cronExpression": "0 0 0 * * *" })
}

#[tokio::test]
async fn background_runs_reject_unauthenticated_callers() {
    let submit = json!({ "workflowId": "report", "input": {} });
    let response =
        app().oneshot(request("POST", "/api/runs", None, Some(submit.clone()))).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response =
        app().oneshot(request("POST", "/api/runs", Some("bad"), Some(submit))).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = app().oneshot(request("GET", "/api/runs/any", None, None)).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn cron_jobs_reject_unauthenticated_callers() {
    let response =
        app().oneshot(request("POST", "/api/cron", None, Some(cron_body()))).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = app().oneshot(request("GET", "/api/cron", None, None)).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn authenticated_callers_reach_the_routes() {
    let response =
        app().oneshot(request("POST", "/api/cron", Some("good"), Some(cron_body()))).await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let response =
        app().oneshot(request("GET", "/api/runs/absent", Some("good"), None)).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND, "authenticated, then a real lookup");
}
