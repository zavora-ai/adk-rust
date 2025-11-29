use adk_core::{Agent, Event, InvocationContext, Result, SingleAgentLoader};
use adk_server::{create_app, ServerConfig};
use adk_session::InMemorySessionService;
use async_trait::async_trait;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use futures::stream;
use std::pin::Pin;
use std::sync::Arc;
use tower::ServiceExt;

struct MockAgent;

#[async_trait]
impl Agent for MockAgent {
    fn name(&self) -> &str {
        "mock-agent"
    }

    fn description(&self) -> &str {
        "Mock Agent"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(
        &self,
        _context: Arc<dyn InvocationContext>,
    ) -> Result<Pin<Box<dyn futures::Stream<Item = Result<Event>> + Send>>> {
        Ok(Box::pin(stream::empty()))
    }
}

#[tokio::test]
async fn test_web_ui_redirect() {
    let agent = Arc::new(MockAgent);
    let agent_loader = Arc::new(SingleAgentLoader::new(agent));
    let session_service = Arc::new(InMemorySessionService::new());
    let config = ServerConfig::new(agent_loader, session_service);
    let app = create_app(config);

    let response = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers().get("location").unwrap(), "/ui/");
}

#[tokio::test]
async fn test_web_ui_assets() {
    let agent = Arc::new(MockAgent);
    let agent_loader = Arc::new(SingleAgentLoader::new(agent));
    let session_service = Arc::new(InMemorySessionService::new());
    let config = ServerConfig::new(agent_loader, session_service);
    let app = create_app(config);

    // Test index.html
    let response = app
        .clone()
        .oneshot(Request::builder().uri("/ui/index.html").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/html"
    );

    // Test runtime-config.json
    let response = app
        .oneshot(
            Request::builder()
                .uri("/ui/assets/config/runtime-config.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body_str = String::from_utf8(body.to_vec()).unwrap();
    assert!(body_str.contains("backendUrl"));
}

#[tokio::test]
async fn test_web_ui_index_route() {
    let agent = Arc::new(MockAgent);
    let agent_loader = Arc::new(SingleAgentLoader::new(agent));
    let session_service = Arc::new(InMemorySessionService::new());
    let config = ServerConfig::new(agent_loader, session_service);
    let app = create_app(config);

    // Test /ui/ serves index.html
    let response = app
        .oneshot(Request::builder().uri("/ui/").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get("content-type").unwrap().to_str().unwrap().contains("text/html"));
}

#[tokio::test]
async fn test_api_apps() {
    let agent = Arc::new(MockAgent);
    let agent_loader = Arc::new(SingleAgentLoader::new(agent));
    let session_service = Arc::new(InMemorySessionService::new());
    let config = ServerConfig::new(agent_loader, session_service);
    let app = create_app(config);

    let response = app
        .oneshot(Request::builder().uri("/api/apps").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body_str = String::from_utf8(body.to_vec()).unwrap();
    assert!(body_str.contains("mock-agent"));
}
