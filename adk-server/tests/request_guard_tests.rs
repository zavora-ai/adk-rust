//! The server answers only the hosts and origins its `SecurityConfig` allows.

use adk_core::{Agent, EventStream, InvocationContext, Result as AdkResult, SingleAgentLoader};
use adk_server::{SecurityConfig, ServerBuilder, ServerConfig, create_app, create_app_with_a2a};
use adk_session::InMemorySessionService;
use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::stream;
use std::sync::Arc;
use tower::ServiceExt;

struct QuietAgent;

#[async_trait]
impl Agent for QuietAgent {
    fn name(&self) -> &str {
        "guarded_agent"
    }

    fn description(&self) -> &str {
        "answers nothing"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> AdkResult<EventStream> {
        Ok(Box::pin(stream::empty()))
    }
}

fn config() -> ServerConfig {
    ServerConfig::new(
        Arc::new(SingleAgentLoader::new(Arc::new(QuietAgent))),
        Arc::new(InMemorySessionService::new()),
    )
}

async fn get(app: &Router, uri: &str, headers: &[(&str, &str)]) -> axum::response::Response {
    let mut request = Request::builder().uri(uri);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    app.clone().oneshot(request.body(Body::empty()).unwrap()).await.unwrap()
}

async fn preflight(app: &Router, origin: &str) -> Option<String> {
    let request = Request::builder()
        .method("OPTIONS")
        .uri("/api/sessions")
        .header("host", "localhost:8080")
        .header("origin", origin)
        .header("access-control-request-method", "POST")
        .header("access-control-request-headers", "content-type")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    response
        .headers()
        .get("access-control-allow-origin")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

#[tokio::test]
async fn a_dns_rebinding_host_is_refused_by_default() {
    let app = create_app(config());
    let sessions = "/api/apps/guarded_agent/users/alice/sessions";

    let response = get(
        &app,
        sessions,
        &[
            ("host", "rebind.attacker.example:8080"),
            ("origin", "http://rebind.attacker.example:8080"),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = get(&app, sessions, &[("host", "rebind.attacker.example:8080")]).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn loopback_names_and_ip_addresses_are_answered_by_default() {
    let app = create_app(config());
    for host in ["localhost:8080", "127.0.0.1:8080", "[::1]:8080", "192.168.1.5:8080"] {
        let response = get(&app, "/api/health", &[("host", host)]).await;
        assert_eq!(response.status(), StatusCode::OK, "host {host}");
    }
}

#[tokio::test]
async fn configured_hosts_are_answered() {
    let app = create_app(config().with_allowed_hosts(vec![
        "agents.example.com".to_string(),
        ".internal.example".to_string(),
    ]));
    for host in ["agents.example.com", "api.internal.example:8443"] {
        let response = get(&app, "/api/health", &[("host", host)]).await;
        assert_eq!(response.status(), StatusCode::OK, "host {host}");
    }
    let response = get(&app, "/api/health", &[("host", "other.example.com")]).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn the_a2a_base_url_host_is_answered() {
    let app = create_app_with_a2a(config(), Some("https://agents.example.com"));
    let response =
        get(&app, "/.well-known/agent-card.json", &[("host", "agents.example.com")]).await;
    assert_eq!(response.status(), StatusCode::OK);

    let response =
        get(&app, "/.well-known/agent-card.json", &[("host", "rebind.attacker.example")]).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_cross_origin_request_is_refused_by_default() {
    let app = create_app(config());
    let response = get(
        &app,
        "/api/apps/guarded_agent/users/alice/sessions",
        &[("host", "localhost:8080"), ("origin", "https://evil.example")],
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    assert_eq!(preflight(&app, "https://evil.example").await, None);
}

#[tokio::test]
async fn a_same_origin_request_is_answered() {
    let app = create_app(config());
    let response = get(
        &app,
        "/api/apps/guarded_agent/users/alice/sessions",
        &[("host", "localhost:8080"), ("origin", "http://localhost:8080")],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn listed_origins_get_cors_access() {
    let app =
        create_app(config().with_allowed_origins(vec!["https://app.example.com".to_string()]));
    let response = get(
        &app,
        "/api/apps/guarded_agent/users/alice/sessions",
        &[("host", "localhost:8080"), ("origin", "https://app.example.com")],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        preflight(&app, "https://app.example.com").await.as_deref(),
        Some("https://app.example.com")
    );
    assert_eq!(preflight(&app, "https://evil.example").await, None);

    let any = create_app(config().with_allowed_origins(vec!["*".to_string()]));
    assert_eq!(preflight(&any, "https://evil.example").await.as_deref(), Some("*"));
}

#[tokio::test]
async fn development_security_admits_a_loopback_frontend() {
    let app = create_app(config().with_security(SecurityConfig::development()));
    assert_eq!(
        preflight(&app, "http://localhost:5173").await.as_deref(),
        Some("http://localhost:5173")
    );
    let response = get(
        &app,
        "/api/apps/guarded_agent/users/alice/sessions",
        &[("host", "localhost:8080"), ("origin", "http://localhost:5173")],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn custom_routes_share_the_guards() {
    let app = ServerBuilder::new(config())
        .add_root_routes(Router::new().route("/custom", axum::routing::get(|| async { "ok" })))
        .add_api_routes(Router::new().route("/projects", axum::routing::get(|| async { "ok" })))
        .build();
    for uri in ["/custom", "/api/projects"] {
        let response = get(&app, uri, &[("host", "rebind.attacker.example")]).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{uri}");
        let response = get(&app, uri, &[("host", "localhost")]).await;
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
    }
}

#[cfg(feature = "agent-engine")]
#[tokio::test]
async fn agent_engine_routes_skip_the_host_check() {
    let app = ServerBuilder::new(config()).with_agent_engine(true).build();
    let request = Request::builder()
        .method("POST")
        .uri("/api/reasoning_engine")
        .header("host", "engine.internal.example")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"class_method":"list_sessions","input":{"user_id":"u"}}"#))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_ne!(response.status(), StatusCode::FORBIDDEN);

    let response = get(&app, "/api/health", &[("host", "engine.internal.example")]).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
