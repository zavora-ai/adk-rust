//! The run routes hand the request context extractor the whole request, and an agent run gets
//! its own timeout (spec 3.1 §8.2).
//!
//! The routes rebuilt a header-only request before calling the extractor, so an extractor that
//! reads the peer address (`ConnectInfo`) saw none on `/api/run*` while it saw one on every
//! other route; and the 30-second request timeout answered `408` while a model turn was still
//! running.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::{
    Agent, EventStream, InvocationContext, RequestContext, Result as AdkResult, SingleAgentLoader,
};
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::{ServerBuilder, ServerConfig};
use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use serde_json::json;
use tower::ServiceExt;

/// Records whether each run was authenticated, after sleeping for `delay`.
struct Recorder {
    delay: Duration,
    seen: Arc<Mutex<Vec<(String, bool)>>>,
}

#[async_trait]
impl Agent for Recorder {
    fn name(&self) -> &str {
        "recorder"
    }
    fn description(&self) -> &str {
        "records the caller"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }
    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
        self.seen.lock().unwrap().push((ctx.user_id().to_string(), ctx.is_authenticated()));
        let delay = self.delay;
        Ok(Box::pin(futures::stream::unfold(false, move |done| async move {
            if !done {
                tokio::time::sleep(delay).await;
            }
            None::<(AdkResult<adk_core::Event>, bool)>
        })))
    }
}

/// Admits loopback peers as `operator`, reading the peer from `ConnectInfo` only.
struct LoopbackPeer;

#[async_trait]
impl RequestContextExtractor for LoopbackPeer {
    async fn extract(
        &self,
        parts: &axum::http::request::Parts,
    ) -> Result<RequestContext, RequestContextError> {
        match parts.extensions.get::<ConnectInfo<SocketAddr>>() {
            Some(ConnectInfo(peer)) if peer.ip().is_loopback() => Ok(RequestContext {
                user_id: "operator".to_string(),
                scopes: vec!["operator".to_string()],
                metadata: Default::default(),
            }),
            _ => Err(RequestContextError::MissingAuth),
        }
    }
}

fn app(config: impl FnOnce(ServerConfig) -> ServerConfig, agent: Recorder) -> Router {
    let loader = Arc::new(SingleAgentLoader::new(Arc::new(agent)));
    let sessions = Arc::new(adk_session::InMemorySessionService::new());
    ServerBuilder::new(config(ServerConfig::new(loader, sessions))).build()
}

fn run_body(user: &str) -> serde_json::Value {
    json!({
        "appName": "recorder",
        "userId": user,
        "sessionId": "s1",
        "newMessage": { "role": "user", "parts": [{ "text": "hi" }] }
    })
}

fn post(uri: &str, body: serde_json::Value, peer: Option<&str>) -> Request<Body> {
    let mut request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    if let Some(peer) = peer {
        request.extensions_mut().insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
    }
    request
}

#[tokio::test]
async fn the_run_routes_extractor_sees_the_peer_address() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let app = app(
        |config| config.with_request_context(Arc::new(LoopbackPeer)),
        Recorder { delay: Duration::ZERO, seen: Arc::clone(&seen) },
    );

    let path_body = json!({ "new_message": "hi" });
    for (uri, body) in [
        ("/api/run", run_body("anyone")),
        ("/api/run_sse", run_body("anyone")),
        ("/api/run/recorder/anyone/s1", path_body),
    ] {
        let response = app.clone().oneshot(post(uri, body.clone(), Some("127.0.0.1:50000"))).await;
        assert_eq!(response.unwrap().status(), StatusCode::OK, "{uri} admits a loopback peer");
        let response = app.clone().oneshot(post(uri, body, Some("203.0.113.9:50000"))).await;
        assert_eq!(response.unwrap().status(), StatusCode::UNAUTHORIZED, "{uri} refuses others");
    }
    // `/api/run` runs before it responds; the streamed routes run as the stream is read.
    let first = seen.lock().unwrap().first().cloned();
    assert_eq!(first, Some(("operator".to_string(), true)), "the run is the authenticated peer");
}

#[tokio::test]
async fn an_agent_run_outlives_the_request_timeout() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let app = app(
        |config| config.with_request_timeout(Duration::from_millis(100)),
        Recorder { delay: Duration::from_millis(400), seen: Arc::clone(&seen) },
    );

    let response = app.oneshot(post("/api/run", run_body("alice"), None)).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK, "a run takes the run timeout, not 100 ms");
    assert_eq!(*seen.lock().unwrap(), vec![("alice".to_string(), false)]);
}

#[tokio::test]
async fn the_run_timeout_bounds_an_agent_run() {
    let app = app(
        |config| config.with_run_timeout(Duration::from_millis(100)),
        Recorder { delay: Duration::from_secs(5), seen: Arc::default() },
    );

    let response = app.oneshot(post("/api/run", run_body("alice"), None)).await.unwrap();

    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    assert_eq!(adk_server::DEFAULT_RUN_TIMEOUT, Duration::from_secs(15 * 60));
}
