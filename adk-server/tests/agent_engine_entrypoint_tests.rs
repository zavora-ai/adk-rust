//! Integration tests for the turnkey Agent Engine entrypoint and the
//! `ServerBuilder` integration.
//!
//! The wave's acceptance criterion is exercised here without a container:
//! the app built by `build_agent_engine_app` must answer the codelab's
//! `query_agent.py` payload at `/api/stream_reasoning_engine` with streamed
//! ADK events.

#![cfg(feature = "agent-engine")]

use adk_server::agent_engine::{AgentEngineOptions, build_agent_engine_app};
use async_stream::stream;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::Arc;
use tower::ServiceExt;

struct EchoAgent;

#[async_trait]
impl adk_core::Agent for EchoAgent {
    fn name(&self) -> &str {
        "echo-agent"
    }

    fn description(&self) -> &str {
        "Echo agent for entrypoint tests"
    }

    fn sub_agents(&self) -> &[Arc<dyn adk_core::Agent>] {
        &[]
    }

    async fn run(
        &self,
        _ctx: Arc<dyn adk_core::InvocationContext>,
    ) -> adk_core::Result<adk_core::EventStream> {
        let s = stream! {
            let mut event = adk_core::Event::new("entrypoint-invocation");
            event.author = "echo-agent".to_string();
            event.llm_response.content =
                Some(adk_core::Content::new("model").with_text("echo: hi"));
            yield Ok(event);
        };
        Ok(Box::pin(s))
    }
}

async fn post(app: &axum::Router, uri: &str, body: serde_json::Value) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// The wave's exit criterion: the codelab payload answered with streamed ADK
/// events, one JSON object per line.
#[tokio::test]
async fn turnkey_app_answers_the_codelab_payload() {
    let app = build_agent_engine_app(Arc::new(EchoAgent), AgentEngineOptions::new()).unwrap();

    let payload = serde_json::json!({
        "class_method": "async_stream_query",
        "input": {"user_id": "u", "message": "hi"}
    });
    let response = post(&app, "/api/stream_reasoning_engine", payload).await;
    assert_eq!(response.status(), StatusCode::OK);
    let content_type =
        response.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap().to_string();
    assert!(content_type.starts_with("application/json"), "content-type was {content_type}");

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let lines: Vec<serde_json::Value> =
        text.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(lines.len(), 1);
    let event: adk_core::Event = serde_json::from_value(lines[0].clone()).unwrap();
    assert_eq!(event.author, "echo-agent");
}

#[tokio::test]
async fn turnkey_app_serves_unary_dispatch_and_health() {
    let app = build_agent_engine_app(Arc::new(EchoAgent), AgentEngineOptions::new()).unwrap();

    let response = post(
        &app,
        "/api/reasoning_engine",
        serde_json::json!({"class_method": "register_operations"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let health = app
        .clone()
        .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
}

#[tokio::test]
async fn app_name_override_scopes_sessions() {
    let app = build_agent_engine_app(
        Arc::new(EchoAgent),
        AgentEngineOptions::new().with_app_name("custom-app"),
    )
    .unwrap();

    let payload = serde_json::json!({
        "class_method": "create_session",
        "input": {"user_id": "u", "session_id": "s-1"}
    });
    let response = post(&app, "/api/reasoning_engine", payload).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["output"]["app_name"], "custom-app");
}

#[tokio::test]
async fn memory_service_option_enables_memory_methods() {
    let app = build_agent_engine_app(
        Arc::new(EchoAgent),
        AgentEngineOptions::new()
            .with_memory_service(Arc::new(adk_memory::InMemoryMemoryService::new())),
    )
    .unwrap();

    // With a memory service configured, search succeeds (empty) instead of 501.
    let payload = serde_json::json!({
        "class_method": "async_search_memory",
        "input": {"user_id": "u", "query": "anything"}
    });
    let response = post(&app, "/api/reasoning_engine", payload).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body, serde_json::json!({"output": {"memories": []}}));
}

// ── ServerBuilder integration ────────────────────────────────────────────

struct SingleAgentLoader;

#[async_trait]
impl adk_core::AgentLoader for SingleAgentLoader {
    async fn load_agent(&self, _app_name: &str) -> adk_core::Result<Arc<dyn adk_core::Agent>> {
        Ok(Arc::new(EchoAgent))
    }

    fn list_agents(&self) -> Vec<String> {
        vec!["echo-agent".to_string()]
    }

    fn root_agent(&self) -> Arc<dyn adk_core::Agent> {
        Arc::new(EchoAgent)
    }
}

#[tokio::test]
async fn server_builder_mounts_the_dispatch_surface() {
    let config = adk_server::ServerConfig::new(
        Arc::new(SingleAgentLoader),
        Arc::new(adk_session::InMemorySessionService::new()),
    );
    let app = adk_server::ServerBuilder::new(config).with_agent_engine(true).build();

    // Dispatch reachable alongside the built-in /api routes.
    let response = post(
        &app,
        "/api/reasoning_engine",
        serde_json::json!({"class_method": "register_operations"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let payload = serde_json::json!({
        "class_method": "async_stream_query",
        "input": {"user_id": "u", "message": "hi"}
    });
    let response = post(&app, "/api/stream_reasoning_engine", payload).await;
    assert_eq!(response.status(), StatusCode::OK);

    // Built-in routes still work next to the merged dispatch routes.
    let health = app
        .clone()
        .oneshot(Request::builder().uri("/api/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
}

#[tokio::test]
async fn server_builder_without_agent_engine_has_no_dispatch_routes() {
    let config = adk_server::ServerConfig::new(
        Arc::new(SingleAgentLoader),
        Arc::new(adk_session::InMemorySessionService::new()),
    );
    let app = adk_server::ServerBuilder::new(config).build();

    let response = post(
        &app,
        "/api/reasoning_engine",
        serde_json::json!({"class_method": "register_operations"}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
