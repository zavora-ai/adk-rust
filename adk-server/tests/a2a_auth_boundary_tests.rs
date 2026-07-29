//! A2A JSON-RPC must sit behind the configured authentication layer.
//!
//! The A2A routes were merged at the router root, outside the layer applied to `/api`, in both
//! `create_app_with_a2a` and `ServerBuilder::build`. With an extractor configured, every other
//! mutation surface required a credential and `/a2a` did not — so any client that could reach
//! the port could drive the agent, call its tools, and incur the cost.
//!
//! Discovery is deliberately still public: an agent card exists to be fetched by peers that
//! have no credential yet.

use adk_core::{Agent, Event, EventStream, InvocationContext, Result};
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use adk_server::{ServerConfig, create_app_with_a2a};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::Arc;
use tower::ServiceExt;

/// A leaf agent; these tests never reach execution.
#[derive(Debug)]
struct TestAgent;

#[async_trait]
impl Agent for TestAgent {
    fn name(&self) -> &str {
        "test_agent"
    }
    fn description(&self) -> &str {
        "agent under an auth boundary"
    }
    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }
    async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
        Ok(Box::pin(futures::stream::empty::<Result<Event>>()))
    }
}

struct TestAgentLoader {
    agent: Arc<dyn Agent>,
}

#[async_trait]
impl adk_core::AgentLoader for TestAgentLoader {
    fn root_agent(&self) -> Arc<dyn Agent> {
        Arc::clone(&self.agent)
    }
    async fn load_agent(&self, name: &str) -> Result<Arc<dyn Agent>> {
        if name == self.agent.name() {
            Ok(Arc::clone(&self.agent))
        } else {
            Err(adk_core::AdkError::agent(format!("Agent not found: {name}")))
        }
    }
    fn list_agents(&self) -> Vec<String> {
        vec![self.agent.name().to_string()]
    }
}

/// Rejects everything, standing in for any real extractor.
#[derive(Debug)]
struct DenyAllExtractor;

#[async_trait]
impl RequestContextExtractor for DenyAllExtractor {
    async fn extract(
        &self,
        _parts: &axum::http::request::Parts,
    ) -> std::result::Result<adk_core::RequestContext, RequestContextError> {
        Err(RequestContextError::MissingAuth)
    }
}

/// A server config with authentication configured.
fn authenticated_config() -> ServerConfig {
    let loader = Arc::new(TestAgentLoader { agent: Arc::new(TestAgent) });
    let sessions = Arc::new(adk_session::InMemorySessionService::new());
    ServerConfig::new(loader, sessions)
        .with_request_context(Arc::new(DenyAllExtractor) as Arc<dyn RequestContextExtractor>)
}

/// A JSON-RPC body valid enough to reach the handler if auth let it through.
fn rpc_body() -> Body {
    Body::from(
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "message/send",
            "params": { "message": { "role": "user", "parts": [{ "kind": "text", "text": "hi" }] } }
        })
        .to_string(),
    )
}

#[tokio::test]
async fn a2a_jsonrpc_requires_authentication() {
    let app = create_app_with_a2a(authenticated_config(), Some("http://localhost:8080"));

    let request = Request::builder()
        .method("POST")
        .uri("/a2a")
        .header("content-type", "application/json")
        .body(rpc_body())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "an unauthenticated caller must not be able to drive the agent through /a2a"
    );
}

#[tokio::test]
async fn a2a_streaming_requires_authentication() {
    let app = create_app_with_a2a(authenticated_config(), Some("http://localhost:8080"));

    let request = Request::builder()
        .method("POST")
        .uri("/a2a/stream")
        .header("content-type", "application/json")
        .body(rpc_body())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "the streaming route executes the same work and needs the same gate"
    );
}

#[tokio::test]
async fn the_agent_card_stays_public() {
    // Discovery must not require a credential: peers fetch it before they have one.
    let app = create_app_with_a2a(authenticated_config(), Some("http://localhost:8080"));

    let request = Request::builder()
        .method("GET")
        .uri("/.well-known/agent.json")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "gating discovery would break A2A peer discovery"
    );
}

#[tokio::test]
async fn a2a_is_reachable_when_no_extractor_is_configured() {
    // Without an extractor there is no authentication to apply, and the layer must not
    // block traffic — otherwise adding the gate breaks every existing deployment.
    let loader = Arc::new(TestAgentLoader { agent: Arc::new(TestAgent) });
    let sessions = Arc::new(adk_session::InMemorySessionService::new());
    let app =
        create_app_with_a2a(ServerConfig::new(loader, sessions), Some("http://localhost:8080"));

    let request = Request::builder()
        .method("POST")
        .uri("/a2a")
        .header("content-type", "application/json")
        .body(rpc_body())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    assert_ne!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "no extractor means no credential to demand"
    );
}

// `A2aServer` is exported only with `a2a-v1`.
#[cfg(feature = "a2a-v1")]
#[tokio::test]
async fn the_default_bind_address_is_loopback() {
    // A server that runs agent work should not publish itself to every interface on build().
    let agent: Arc<dyn Agent> = Arc::new(TestAgent);
    let app = adk_server::A2aServer::builder().agent(agent).build().expect("build");

    assert!(
        app.bind_addr().starts_with("127.0.0.1"),
        "default bind must be loopback, got {}",
        app.bind_addr()
    );
}
